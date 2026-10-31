use super::*;
use krill_proto::screen::{Cell, Cursor};
use std::collections::BTreeMap as Env;
use std::fs;
use std::os::unix::fs::PermissionsExt as _;

const MODELS: &str = "Fetching available models...
gemini-4-argon-medium\tGemini 4 Argon (Medium)
gemini-4-argon-high\tGemini 4 Argon (High)
gemini-4-argon-low\tGemini 4 Argon (Low)
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
            let efforts: Vec<_> = family.levels.iter().map(|l| l.effort.as_str()).collect();
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
    assert_eq!(families[1].level("low").unwrap().id, "gemini-3.8-flash-low");
    assert_eq!(
        families[0].item(),
        json!({"id":"gemini-4-argon","label":"Gemini 4 Argon","efforts":["low","medium","high"]})
    );
}

#[test]
fn rows_that_do_not_encode_their_effort_are_left_out() {
    for row in [
        "plain-model\tPlain Model",
        "gemini-x-low\tGemini X (High)",
        "gemini-x-turbo\tGemini X (Turbo)",
        "bad id-low\tBad (Low)",
        "../x-low\tEscape (Low)",
        "x-low\tRm <rf> (Low)",
    ] {
        assert!(parse(row).is_empty(), "{row}");
    }
}

#[test]
fn both_footer_layouts_give_label_and_effort() {
    let pair = |label: &str, effort: &str| Some((label.to_owned(), effort.to_owned()));
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
    for other in [
        "  ↑/↓ Navigate · enter Select",
        "assistant text (not an effort)",
        "> Gemini 3.8 Flash (Low)  gemini-3.8-flash-low",
        "",
    ] {
        assert_eq!(footer(&screen(&[other], (0, 0))), None, "{other}");
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
        Some(("Gemini 4 Argon".to_owned(), "medium".to_owned()))
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
        Some(("Gemini 3.8 Flash".to_owned(), "high".to_owned()))
    );
}

#[test]
fn only_an_empty_composer_between_borders_is_idle() {
    assert!(composer(&idle(STATUS), None));
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
    let target = families[1].level("low").unwrap();
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
            &with("> Gemini 3.8 Flash (Low)  gemini-3.8-fla..."),
            command,
            target
        ),
        "44-column terminal"
    );
    for wrong in [
        "> Gemini 3.8 Flash (High)  gemini-3.8-flash-high",
        "> Gemini 3.8 Flash (Low)  gemini-4-arg...",
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
    assert_eq!(families.len(), 4);
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
fn a_poll_never_waits_for_the_listing() {
    let (root, context) = fixture("poll", "1.4.12", "1").unwrap();
    let first = Agy.catalog(&context);
    assert_eq!(first["items"], json!([]));
    assert_eq!(first["reason"], LOADING);
    Agy.settle(&context);
    let ready = Agy.catalog(&context);
    assert_eq!(ready["items"].as_array().unwrap().len(), 4, "{ready}");
    assert_eq!(ready["supported"], false, "listing alone grants no control");
    fs::remove_dir_all(root).unwrap();
}
