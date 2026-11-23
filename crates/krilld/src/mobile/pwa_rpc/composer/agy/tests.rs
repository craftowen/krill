use super::*;
use krill_proto::screen::{Cell, Color, Cursor, Flags};
use std::collections::BTreeMap as Env;
use std::fs;
use std::os::unix::fs::PermissionsExt as _;

const MODELS: &str = "Fetching available models...
gemini-4-argon-medium\tGemini 4 Argon (Medium)
gemini-4-argon-high\tGemini 4 Argon (High)
gemini-4-argon-low\tGemini 4 Argon (Low)
gemini-pro-orchestrator-neon\tOrchestrator (Neon)
gemini-pro-orchestrator\tOrchestrator (Carbon)
gemini-pro-orchestrator-argon\tOrchestrator (Argon)
gemini-3.8-flash-high\tGemini 3.8 Flash (High)
gemini-3.8-flash-medium\tGemini 3.8 Flash (Medium)
gemini-3.8-flash-low\tGemini 3.8 Flash (Low)
claude-opus-5-5-low\tClaude Opus 5.5 (Low)
claude-opus-5-5-medium\tClaude Opus 5.5 (Medium)
claude-opus-5-5-high\tClaude Opus 5.5 (High)
gpt-oss-120b-medium\tGPT-OSS 120B (Medium)
";
const RULE: &str = "────────────────────────────────────────";
const STATUS: &str =
    "Gemini 4 Argon (Medium) (1.0M) │ 미니멀리즘* │ 0/1.0M │ 5h 22% (3h 34m) │ 7d 15% (6d 16h)";

fn screen(lines: &[&str], cursor: (u16, u16)) -> Screen {
    Screen {
        cols: 100,
        lines: 32,
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
            line: cursor.0,
            column: cursor.1,
            visible: true,
        },
        ..Screen::default()
    }
}

fn idle(footer: &str) -> Screen {
    screen(
        &["   Antigravity CLI 1.4.12", RULE, ">", RULE, footer, "", ""],
        (2, 2),
    )
}

#[test]
fn rows_group_by_model_with_ordered_efforts() {
    let families = parse(MODELS);
    let summary: Vec<_> = families
        .iter()
        .map(|family| {
            let efforts: Vec<_> = family
                .levels
                .iter()
                .filter_map(|l| l.effort.as_deref())
                .collect();
            (family.id.as_str(), family.label.as_str(), efforts)
        })
        .collect();
    assert_eq!(
        summary,
        [
            (
                "gemini-4-argon",
                "Gemini 4 Argon",
                vec!["low", "medium", "high"]
            ),
            (
                "gemini-pro-orchestrator-neon",
                "Orchestrator (Neon)",
                vec![]
            ),
            ("gemini-pro-orchestrator", "Orchestrator (Carbon)", vec![]),
            (
                "gemini-pro-orchestrator-argon",
                "Orchestrator (Argon)",
                vec![]
            ),
            (
                "gemini-3.8-flash",
                "Gemini 3.8 Flash",
                vec!["low", "medium", "high"]
            ),
            (
                "claude-opus-5-5",
                "Claude Opus 5.5",
                vec!["low", "medium", "high"]
            ),
            ("gpt-oss-120b", "GPT-OSS 120B", vec!["medium"]),
        ]
    );
    assert_eq!(
        families[4].level(Some("low")).unwrap().id,
        "gemini-3.8-flash-low"
    );
    assert_eq!(
        families[1].level(None).unwrap().id,
        "gemini-pro-orchestrator-neon"
    );
    assert!(families[1].level(Some("high")).is_none());
    assert_eq!(
        families[0].item(),
        json!({"id":"gemini-4-argon","label":"Gemini 4 Argon","efforts":["low","medium","high"]})
    );
    assert_eq!(
        families[1].item(),
        json!({"id":"gemini-pro-orchestrator-neon","label":"Orchestrator (Neon)","efforts":[]})
    );
}

#[test]
fn invalid_rows_and_mismatched_efforts_are_rejected_while_standalone_models_parse() {
    for row in [
        "gemini-x-low\tGemini X (High)",
        "bad id-low\tBad (Low)",
        "../x-low\tEscape (Low)",
        "x-low\tRm <rf> (Low)",
        "nested\tBad ((Inner))",
        "bad-url\tBad (http://x)",
    ] {
        assert!(parse(row).is_empty(), "{row}");
    }
    let parsed =
        parse("plain-model\tPlain Model\ngemini-pro-orchestrator-neon\tOrchestrator (Neon)");
    assert_eq!(parsed.len(), 2);
    assert_eq!(parsed[0].id, "plain-model");
    assert_eq!(parsed[1].label, "Orchestrator (Neon)");
    assert!(parsed[1].levels[0].effort.is_none());
}

#[test]
fn both_footer_layouts_and_standalone_orchestrator_models_parse() {
    let pair = |label: &str, effort: &str| Some((label.to_owned(), Some(effort.to_owned())));
    let bare = |label: &str| Some((label.to_owned(), None));
    let families = parse(MODELS);
    assert_eq!(
        footer(&idle(STATUS)),
        pair("Gemini 4 Argon", "medium"),
        "status line"
    );
    assert_eq!(
        footer(&idle(&format!("{}Gemini 4 Argon · medium", " ".repeat(60)))),
        pair("Gemini 4 Argon", "medium"),
        "default right-aligned footer"
    );
    assert_eq!(
        footer(&idle("Gemini 3.8 Flash (High)")),
        pair("Gemini 3.8 Flash", "high")
    );
    assert_eq!(
        footer(&idle("Gemini 4 Argon (Medium) (1.0M) │ 미니멀리즘*")),
        pair("Gemini 4 Argon", "medium"),
        "cut by a narrow terminal"
    );
    assert_eq!(
        footer(&idle("GPT-OSS 120B · medium")),
        pair("GPT-OSS 120B", "medium")
    );
    assert_eq!(
        footer(&idle(
            "Orchestrator (Neon) (1.0M) │ main* │ 245.7k/1.0M │ 5h 2% (4h 49m)"
        )),
        bare("Orchestrator (Neon)"),
        "standalone orchestrator in default status line"
    );
    assert_eq!(
        footer_with(
            &idle("Orchestrator (Neon) (1M) │ 993.6k/1.0M"),
            Some(&families)
        ),
        bare("Orchestrator (Neon)"),
        "1M context window suffix stripped"
    );
    assert_eq!(
        footer_with(
            &idle(&format!("{}Orchestrator (Neon)", " ".repeat(60))),
            Some(&families)
        ),
        bare("Orchestrator (Neon)"),
        "right-aligned standalone model with known catalog"
    );
    assert_eq!(
        footer_with(
            &idle("? for shortcuts    auto  · Gemini 3.8 Flash · low"),
            Some(&families)
        ),
        pair("Gemini 3.8 Flash", "low"),
        "default footer without custom statusLine"
    );
    assert_eq!(
        footer_with(
            &idle("esc to cancel    auto  · Gemini 4 Argon · xhigh"),
            Some(&families)
        ),
        pair("Gemini 4 Argon", "xhigh"),
        "active generation footer without custom statusLine"
    );
    assert_eq!(
        footer_with(
            &idle("? for shortcuts    auto  · Orchestrator (Neon)"),
            Some(&families)
        ),
        bare("Orchestrator (Neon)"),
        "default footer with standalone orchestrator"
    );
    assert!(composer_ready(&idle(
        "? for shortcuts    auto  · Gemini 3.8 Flash · low"
    )));
    assert!(!composer_ready(&screen(&["Signing in..."], (0, 13))));
    for other in [
        "  ↑/↓ Navigate · enter Select",
        "assistant text (not an effort)",
        "> Gemini 3.8 Flash (Low)  gemini-3.8-flash-low",
        "",
    ] {
        assert_eq!(
            footer_with(&screen(&[other], (0, 0)), Some(&families)),
            None,
            "{other}"
        );
    }
}

#[test]
fn footer_stays_readable_above_an_open_completion_row() {
    let open = screen(
        &[
            RULE,
            "> /model gemini-3.8-flash-low",
            RULE,
            "> Gemini 3.8 Flash (Low)  gemini-3.8-flash-low",
            "",
            "  ↑/↓ Navigate · enter Select · tab Complete",
            STATUS,
            "",
        ],
        (1, 29),
    );
    assert_eq!(
        footer(&open),
        Some(("Gemini 4 Argon".to_owned(), Some("medium".to_owned())))
    );
}

#[test]
fn footer_stays_readable_while_the_agent_generates() {
    let working = screen(
        &[
            "> /model",
            "  ⎿  Model set to Gemini 3.8 Flash (High)",
            "⣷  Generating...",
            RULE,
            ">",
            RULE,
            "Gemini 3.8 Flash (High) (1.0M) │ 미니멀리즘* │ 0/1.0M │ 5h 24% (3h 7m) │ 7d 16% (6d 15h)",
            "",
        ],
        (4, 2),
    );
    assert_eq!(
        footer(&working),
        Some(("Gemini 3.8 Flash".to_owned(), Some("high".to_owned())))
    );
}

#[test]
fn only_an_empty_composer_or_mode_placeholder_between_borders_is_idle() {
    assert!(composer(&idle(STATUS), None));

    let prompt_fg = Color::Rgb(krill_proto::theme::Rgb {
        r: 122,
        g: 162,
        b: 247,
    });
    let hint_fg = Color::Rgb(krill_proto::theme::Rgb {
        r: 154,
        g: 165,
        b: 206,
    });
    let mut hinted = idle(STATUS);
    hinted.rows[2] = "> Auto mode: edits & safe commands auto-approved (shift+tab to cycle)"
        .chars()
        .enumerate()
        .map(|(idx, ch)| Cell {
            ch,
            fg: if idx == 0 {
                prompt_fg
            } else if idx >= 2 {
                hint_fg
            } else {
                Color::Default
            },
            ..Cell::default()
        })
        .collect();
    assert!(
        composer(&hinted, None),
        "agy 1.5.13 themed mode placeholder at column 2 is idle"
    );

    let mut truncated_40col = idle(STATUS);
    truncated_40col.rows[2] = "> Auto mode: edits & safe commands a..."
        .chars()
        .enumerate()
        .map(|(idx, ch)| Cell {
            ch,
            fg: if idx == 0 {
                prompt_fg
            } else if idx >= 2 {
                hint_fg
            } else {
                Color::Default
            },
            ..Cell::default()
        })
        .collect();
    assert!(composer(&truncated_40col, None));

    let mut dim_placeholder = idle(STATUS);
    dim_placeholder.rows[2] = "> Type your message..."
        .chars()
        .enumerate()
        .map(|(idx, ch)| Cell {
            ch,
            flags: if idx >= 2 {
                Flags::DIM
            } else {
                Flags::default()
            },
            ..Cell::default()
        })
        .collect();
    assert!(composer(&dim_placeholder, None));

    let mut slash_accent = idle(STATUS);
    slash_accent.rows[2] = "> /help"
        .chars()
        .enumerate()
        .map(|(idx, ch)| Cell {
            ch,
            fg: if idx == 0 || idx >= 2 {
                prompt_fg
            } else {
                Color::Default
            },
            ..Cell::default()
        })
        .collect();
    assert!(
        !composer(&slash_accent, None),
        "slash command is not placeholder"
    );

    let mut hidden = idle(STATUS);
    hidden.cursor.visible = false;
    assert!(!composer(&hidden, None), "hidden cursor");
    let mut moved = idle(STATUS);
    moved.cursor.column = 3;
    assert!(!composer(&moved, None), "cursor off the prompt");
    for row in ["> draft", "! sleep 7", "> Yes, I trust this folder"] {
        let busy = screen(&[RULE, row, RULE, STATUS], (1, 2));
        assert!(!composer(&busy, None), "{row}");
    }
    let unbounded = screen(&["text", ">", RULE, STATUS], (1, 2));
    assert!(!composer(&unbounded, None), "missing top border");
    let picker = screen(&["> Yes, I trust this folder", "  No, exit"], (0, 2));
    assert!(!composer(&picker, None), "consent screen");
}

#[test]
fn enter_needs_the_echo_and_the_exact_completion_row() {
    let families = parse(MODELS);
    let target = families[4].level(Some("low")).unwrap();
    let command = "/model gemini-3.8-flash-low";
    let with = |completion: &str| {
        screen(
            &[RULE, &format!("> {command}"), RULE, completion, "", STATUS],
            (1, 29),
        )
    };
    assert!(echoed(
        &with("> Gemini 3.8 Flash (Low)  gemini-3.8-flash-low"),
        command,
        target
    ));
    assert!(
        echoed(
            &with("> Gemini 3.8 Flash (Low)      gemini-3.8-flash-low"),
            command,
            target
        ),
        "agy 1.5.13 column-aligned 6-space padding"
    );
    assert!(
        echoed(
            &with("> Gemini 3.8 Flash (Low)  gemini-3.8-fla..."),
            command,
            target
        ),
        "44-column terminal"
    );
    assert!(
        echoed(&with("> Gemini 3.8 Flash (Low)"), command, target),
        "40-column terminal omitting id column"
    );
    let orch_carbon = families[2].level(None).unwrap();
    let orch_cmd = "/model gemini-pro-orchestrator";
    let multi_row = screen(
        &[
            RULE,
            &format!("> {orch_cmd}"),
            RULE,
            "  Orchestrator (Neon)         gemini-pro-orchestrator-neon",
            "> Orchestrator (Carbon)       gemini-pro-orchestrator",
            "  Orchestrator (Argon)        gemini-pro-orchestrator-argon",
            "",
            "  ↑/↓ Navigate · enter Select · tab Complete",
            STATUS,
        ],
        (1, 32),
    );
    assert!(
        echoed(&multi_row, orch_cmd, orch_carbon),
        "exact Orchestrator (Carbon) highlighted on second row"
    );
    for wrong in [
        "> Gemini 3.8 Flash (High)  gemini-3.8-flash-high",
        "> Gemini 3.8 Flash (Low)  gemini-4-arg...",
        "> Gemini 3.8 Flash (Low) extra",
        "> /model  Set a model, or run a single prompt on another model",
        "",
    ] {
        assert!(!echoed(&with(wrong), command, target), "{wrong}");
    }
    let other_draft = screen(
        &[
            RULE,
            "> /model gemini-4-argon-low",
            RULE,
            "> Gemini 3.8 Flash (Low)  gemini-3.8-flash-low",
        ],
        (1, 28),
    );
    assert!(!echoed(&other_draft, command, target));
}

#[test]
fn only_the_latest_native_line_confirms() {
    let after = |line: &str| screen(&[line, "", RULE, ">", RULE, STATUS], (3, 2));
    let label = "Gemini 3.8 Flash (Low)";
    assert!(confirmed(
        &after("  ⎿  Model set to Gemini 3.8 Flash (Low)"),
        label
    ));
    assert!(confirmed(
        &after("  ⎿  Model already set to Gemini 3.8 Flash (Low)"),
        label
    ));
    for line in [
        "  ⎿  Model set to Gemini 4 Argon (Medium)",
        "Model set to Gemini 3.8 Flash (Low)",
        "  ⎿  Effort set to high",
        "",
    ] {
        assert!(!confirmed(&after(line), label), "{line}");
    }
    let stale = screen(
        &[
            "  ⎿  Model set to Gemini 3.8 Flash (Low)",
            "newer output",
            RULE,
            ">",
            RULE,
        ],
        (3, 2),
    );
    assert!(!confirmed(&stale, label));
    let callout_and_wrap = screen(
        &[
            "> /model",
            "  ⎿  Model set to Gemini 4 Carbon",
            "     (Medium)",
            " ┃ Introducing Gemini 4 Carbon",
            " ┃ Our most advanced model yet!",
            " ┃ esc to dismiss",
            RULE,
            ">",
            RULE,
            "Gemini 4 Carbon (Medium) (1.0M) │ 0/1.0M",
        ],
        (7, 2),
    );
    assert!(
        confirmed(&callout_and_wrap, "Gemini 4 Carbon (Medium)"),
        "narrow wrapped acknowledgment above sticky callout banner confirms"
    );
    assert!(!confirmed(&callout_and_wrap, "Gemini 4 Carbon (High)"));
    let stale_behind_callout = screen(
        &[
            "  ⎿  Model set to Gemini 4 Carbon (Medium)",
            "> /help",
            " ┃ Introducing Gemini 4 Carbon",
            " ┃ esc to dismiss",
            RULE,
            ">",
            RULE,
        ],
        (5, 2),
    );
    assert!(!confirmed(
        &stale_behind_callout,
        "Gemini 4 Carbon (Medium)"
    ));
}

#[test]
fn a_visible_banner_outranks_the_installed_version() {
    assert!(verified(&idle(STATUS)));
    let scrolled = screen(&[RULE, ">", RULE, STATUS], (1, 2));
    assert!(
        verified(&scrolled),
        "banner gone: installed version decides"
    );
    let older = screen(
        &["   Antigravity CLI 1.4.11", RULE, ">", RULE, STATUS],
        (2, 2),
    );
    assert!(!verified(&older));
    let newer = screen(
        &["   Antigravity CLI 1.5.0", RULE, ">", RULE, STATUS],
        (2, 2),
    );
    assert!(verified(&newer));
    let prose = screen(
        &["Antigravity CLI understands your codebase", RULE, ">"],
        (2, 2),
    );
    assert!(verified(&prose), "prose is not a version banner");
}

fn fixture(tag: &str, version: &str, delay: &str) -> io::Result<(PathBuf, Context)> {
    let root = std::env::temp_dir().join(format!("krill-agy-{tag}-{}", std::process::id()));
    fs::remove_dir_all(&root).ok();
    let bin = root.join("home/.local/bin");
    fs::create_dir_all(&bin)?;
    let exe = bin.join("agy");
    let script = format!(
        "#!/bin/sh\nif [ \"$1\" = --version ]; then echo {version}; exit 0; fi\n\
         test \"$SSH_CONNECTION\" = krill && test \"$1\" = \"--gemini_dir={}/account/.gemini\" && \
         test \"$2\" = models || exit 1\nsleep {delay}\ncat <<'ROWS'\n{MODELS}ROWS\n",
        root.display()
    );
    fs::write(&exe, script)?;
    fs::set_permissions(&exe, fs::Permissions::from_mode(0o755))?;
    let context = Context {
        home: root.join("home"),
        cwd: root.clone(),
        agent: "agy".into(),
        config: Some(root.join("account")),
        env: Env::new(),
    };
    Ok((root, context))
}

#[test]
fn listing_uses_the_session_profile_and_the_home_install() {
    let (root, context) = fixture("fetch", "1.4.12", "0").unwrap();
    let families = fetch(context.config.as_deref(), &context.home).unwrap();
    assert_eq!(families.len(), 7);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn a_version_below_the_floor_never_lists_and_says_why() {
    let (root, context) = fixture("version", "1.4.11", "0").unwrap();
    assert!(matches!(
        fetch(context.config.as_deref(), &context.home),
        Err(UNVERIFIED)
    ));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn a_future_version_lists() {
    let (root, context) = fixture("future", "1.5.0", "0").unwrap();
    assert!(fetch(context.config.as_deref(), &context.home).is_ok());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn a_missing_install_says_why() {
    let root = std::env::temp_dir().join(format!("krill-agy-none-{}", std::process::id()));
    assert!(matches!(fetch(None, &root), Err(NOT_INSTALLED)));
}

#[test]
fn a_new_bot_gets_every_row_with_the_effort_in_its_id() {
    let (root, context) = fixture("bot", "1.4.12", "0").unwrap();
    Agy.settle(&context);
    let listed = launch_models(&context);
    assert_eq!(listed.reason, None);
    assert_eq!(listed.models.len(), 13, "{listed:?}");
    assert_eq!(
        listed.models[0],
        BotAgentModel {
            id: "gemini-4-argon-low".into(),
            label: "Gemini 4 Argon (Low)".into(),
        }
    );
    assert!(
        listed
            .models
            .iter()
            .any(|m| m.id == "gemini-pro-orchestrator")
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn a_poll_never_waits_for_the_listing() {
    let (root, context) = fixture("poll", "1.4.12", "1").unwrap();
    let first = Agy.catalog(&context);
    assert_eq!(first["items"], json!([]));
    assert_eq!(first["reason"], LOADING);
    Agy.settle(&context);
    let ready = Agy.catalog(&context);
    assert_eq!(ready["items"].as_array().unwrap().len(), 7, "{ready}");
    assert_eq!(ready["supported"], false, "listing alone grants no control");
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn stops_waiting_listing_server_before_launch_gate_changes_account() {
    // Mirrors Orca `acp-listing-session-models.test.ts` (#46647): an in-flight listing must be
    // stopped before an account sign-in/switch mutates credentials, concurrent listings during
    // the op must fail as Stopped (not cached as probe failure), and subsequent listings succeed.
    let root = std::env::temp_dir().join(format!("krill-agy-gate-{}", std::process::id()));
    fs::remove_dir_all(&root).ok();
    let bin = root.join("home/.local/bin");
    fs::create_dir_all(&bin).unwrap();
    let started_flag = root.join("started");
    let exe = bin.join("agy");
    let script = format!(
        "#!/bin/sh\nif [ \"$1\" = --version ]; then echo 1.5.14; exit 0; fi\n\
         touch '{}'\n\
         sleep 30\n",
        started_flag.display()
    );
    fs::write(&exe, script).unwrap();
    fs::set_permissions(&exe, fs::Permissions::from_mode(0o755)).unwrap();
    let cfg = root.join("account");
    let home = root.join("home");

    let cfg_bg = cfg.clone();
    let home_bg = home.clone();
    let handle = std::thread::spawn(move || fetch_gated(Some(&cfg_bg), &home_bg));

    let wait_deadline = Instant::now() + Duration::from_secs(5);
    while !started_flag.exists() && Instant::now() < wait_deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(started_flag.exists(), "listing process should have started");

    let mut refused = None;
    direct::agy::run_account_op(|| {
        // New listing while gate is held must be refused as Stopped, never a probe failure.
        refused = Some(fetch_gated(Some(&cfg), &home));
    });
    assert_eq!(refused, Some(Err(FetchError::Stopped)));

    let stopped = handle.join().unwrap();
    assert_eq!(
        stopped,
        Err(FetchError::Stopped),
        "in-flight listing must be stopped cleanly by the gate"
    );

    // Refresh via gate-stopped error must not cache a probe failure.
    let cfg_opt = Some(cfg.clone());
    lock(&CACHE).remove(&cfg_opt);
    direct::agy::run_account_op(|| {
        refresh(cfg_opt.clone(), &home);
    });
    {
        let cache = lock(&CACHE);
        let slot = cache.get(&cfg_opt);
        assert!(
            slot.and_then(|s| s.failure).is_none(),
            "gate refusal must not be cached as a probe failure"
        );
    }
    fs::remove_dir_all(root).ok();
}

#[test]
fn fold_listed_family_maps_multi_level_ids_and_keeps_standalone_ids() {
    // Mirrors Orca `antigravity-model-listing.test.ts` (#46941).
    let families = parse(MODELS);
    let listed = to_listed_models(&families);
    assert_eq!(
        krill_proto::agent::antigravity_listed_family_selection(
            "gemini-4-argon-medium",
            Some("medium"),
            &listed
        ),
        Some(("gemini-4-argon".to_owned(), "medium".to_owned()))
    );
    assert_eq!(
        krill_proto::agent::antigravity_listed_family_selection(
            "gemini-4-argon-medium",
            None,
            &listed
        ),
        Some(("gemini-4-argon".to_owned(), "medium".to_owned()))
    );
    assert_eq!(
        krill_proto::agent::antigravity_listed_family_selection(
            "gemini-4-argon-medium",
            Some("high"),
            &listed
        ),
        Some(("gemini-4-argon".to_owned(), "high".to_owned()))
    );
    assert_eq!(
        krill_proto::agent::antigravity_listed_family_selection(
            "gemini-4-argon-medium",
            Some("xhigh"),
            &listed
        ),
        None
    );
    assert_eq!(
        krill_proto::agent::antigravity_listed_family_selection(
            "gpt-oss-120b-medium",
            Some("medium"),
            &listed
        ),
        None
    );
    assert_eq!(
        krill_proto::agent::antigravity_listed_family_selection(
            "gemini-pro-orchestrator-neon",
            None,
            &listed
        ),
        None
    );
}

#[test]
fn classify_version_output_handles_leading_v_and_ignores_filenames() {
    assert_eq!(classify_version_output("agy v1.4.12\n"), VerifiedExe::Ok);
    assert_eq!(
        classify_version_output("Antigravity CLI V1.5.14\n"),
        VerifiedExe::Ok
    );
    assert_eq!(
        classify_version_output("agy.exe (flow.google.com) v1.5.0\n"),
        VerifiedExe::Ok
    );
    assert_eq!(
        classify_version_output("agy v1.3.9\n"),
        VerifiedExe::TooOld("1.3.9".to_owned())
    );
    assert_eq!(
        classify_version_output("agy.exe flow.google.com\n"),
        VerifiedExe::Failed(UNVERIFIED)
    );
}

#[test]
fn effort_supported_is_false_when_session_control_is_unsupported() {
    let families = parse(MODELS);
    let multi = &families[0]; // Gemini 4 Argon (low/medium/high)
    let standalone = &families[1]; // Orchestrator (Neon) (no efforts)
    assert!(!agy_effort_supported(false, Some(multi), Some("medium")));
    assert!(!agy_effort_supported(false, None, Some("high")));
    assert!(!agy_effort_supported(false, Some(standalone), Some("low")));
    assert!(agy_effort_supported(true, Some(multi), None));
    assert!(agy_effort_supported(true, Some(standalone), Some("medium")));
    assert!(!agy_effort_supported(true, Some(standalone), None));
}
