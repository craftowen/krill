//! Source lint kept in the crate so the normal test gate rejects untranslated UI copy.
#![expect(
    clippy::unwrap_used,
    reason = "source lint test helpers operate on compiled Rust source"
)]

use std::fs;
use std::path::Path;

#[derive(Debug)]
struct Token<'a> {
    value: &'a str,
    literal: bool,
    offset: usize,
}

// Rust strings/comments are scanned rather than lines: multiline and raw strings must
// obey the same rule, while comments, char literals, and lint reasons are not UI copy.
fn tokens(source: &str) -> Vec<Token<'_>> {
    let bytes = source.as_bytes();
    let mut result = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_whitespace() {
            i += 1;
        } else if bytes[i..].starts_with(b"//") {
            i += source
                .get(i..)
                .unwrap()
                .find('\n')
                .unwrap_or(bytes.len() - i);
        } else if bytes[i..].starts_with(b"/*") {
            i += 2;
            let mut depth = 1;
            while i < bytes.len() && depth > 0 {
                if bytes[i..].starts_with(b"/*") {
                    depth += 1;
                    i += 2;
                } else if bytes[i..].starts_with(b"*/") {
                    depth -= 1;
                    i += 2;
                } else {
                    i += 1;
                }
            }
        } else if bytes[i] == b'\''
            && let Some(end) = source
                .get(i + 1..)
                .unwrap()
                .find('\'')
                .filter(|end| *end < 12)
        {
            i += end + 2;
        } else if bytes[i] == b'"' {
            let start = i;
            i += 1;
            while i < bytes.len() && bytes[i] != b'"' {
                i += if bytes[i] == b'\\' { 2 } else { 1 };
            }
            i = (i + 1).min(bytes.len());
            result.push(Token {
                value: source.get(start + 1..i - 1).unwrap(),
                literal: true,
                offset: start,
            });
        } else if bytes[i] == b'r'
            && let Some(hashes) = bytes[i + 1..].iter().position(|b| *b != b'#')
            && bytes.get(i + 1 + hashes) == Some(&b'"')
        {
            let start = i;
            let content = i + 2 + hashes;
            let ending = format!("\"{}", "#".repeat(hashes));
            let end = source.get(content..).unwrap().find(&ending).unwrap() + content;
            i = end + ending.len();
            result.push(Token {
                value: source.get(content..end).unwrap(),
                literal: true,
                offset: start,
            });
        } else if bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_' {
            let start = i;
            while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                i += 1;
            }
            result.push(Token {
                value: source.get(start..i).unwrap(),
                literal: false,
                offset: start,
            });
        } else {
            let len = source.get(i..).unwrap().chars().next().unwrap().len_utf8();
            result.push(Token {
                value: source.get(i..i + len).unwrap(),
                literal: false,
                offset: i,
            });
            i += len;
        }
    }
    result
}

fn violations(source: &str) -> Vec<usize> {
    let code = tokens(source);
    let mut violations = Vec::new();
    for (i, token) in code.iter().enumerate().filter(|(_, t)| t.literal) {
        let previous: Vec<_> = code[..i].iter().rev().take(7).map(|t| t.value).collect();
        if previous.starts_with(&["=", "reason"]) {
            continue;
        }
        let hangul = token
            .value
            .chars()
            .any(|c| ('\u{ac00}'..='\u{d7a3}').contains(&c));
        let sink = |name: &str| {
            matches!(
                name,
                "child"
                    | "label"
                    | "tooltip"
                    | "placeholder"
                    | "set_placeholder"
                    | "accessibility_label"
                    | "aria_label"
                    | "set_window_title"
                    | "set_title"
            )
        };
        let direct_ui =
            previous.first() == Some(&"(") && previous.get(1).is_some_and(|name| sink(name));
        let formatted_ui = previous.starts_with(&["(", "!", "format", "("])
            && previous.get(4).is_some_and(|name| sink(name));
        let menu = previous.starts_with(&["(", "action", ":", ":", "MenuItem"])
            || previous.starts_with(&["(", "new", ":", ":", "Menu"]);
        let visible = token
            .value
            .split('{')
            .enumerate()
            .map(|(i, part)| {
                if i == 0 {
                    part
                } else {
                    part.split_once('}').map_or(part, |(_, tail)| tail)
                }
            })
            .collect::<String>();
        if hangul
            || ((direct_ui || formatted_ui || menu) && visible.chars().any(char::is_alphabetic))
        {
            violations.push(source.get(..token.offset).unwrap().lines().count());
        }
    }
    violations
}

fn check_dir(path: &Path, failures: &mut Vec<String>) {
    for entry in fs::read_dir(path).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            check_dir(&path, failures);
        } else if path.extension().is_some_and(|ext| ext == "rs")
            && path
                .file_name()
                .is_some_and(|name| name != "i18n.rs" && !name.to_string_lossy().contains("tests"))
        {
            let source = fs::read_to_string(&path).unwrap();
            let production = source.split("#[cfg(test)]").next().unwrap();
            for line in violations(production) {
                failures.push(format!("{}:{line}", path.display()));
            }
        }
    }
}

#[test]
fn app_ui_copy_uses_the_catalog() {
    let mut failures = Vec::new();
    check_dir(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
        &mut failures,
    );
    assert!(
        failures.is_empty(),
        "UI literals must use i18n::catalog:\n{}",
        failures.join("\n")
    );
}

#[test]
fn lint_rejects_both_languages_and_raw_or_formatted_ui_literals() {
    for source in [
        r#"div().child("하드코딩")"#,
        r#"Button::new("id").label("Settings")"#,
        r##"input.placeholder(r#"Search"#)"##,
        r#"div().child(format!("Hello {name}"))"#,
        r#"MenuItem::action("Close", action)"#,
        r#"let x = "여러\n줄";"#,
    ] {
        assert!(!violations(source).is_empty(), "{source}");
    }
    assert!(violations("// .label(\"설정\")\n/* nested /* 설정 */ comment */\n#[expect(lint, reason = \"근거\")] fn f() {}\nlet quote = '\"'; div().id(\"settings\").child(text::TITLE.get());").is_empty());
}
