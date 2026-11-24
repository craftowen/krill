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
    end: usize,
}

fn has_hangul(s: &str) -> bool {
    s.chars().any(|c| {
        ('\u{ac00}'..='\u{d7a3}').contains(&c)
            || ('\u{1100}'..='\u{11ff}').contains(&c)
            || ('\u{3130}'..='\u{318f}').contains(&c)
    })
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
        } else if bytes[i] == b'\'' {
            // Distinguish Rust char literals (`'a'`, `'한'`, `'\''`, `'\u{1100}'`) from lifetimes
            // (`'a`, `'static`) so escaped quotes never desynchronize string scanning.
            let mut end = None;
            if i + 1 < bytes.len() && bytes[i + 1] == b'\\' {
                let mut k = i + 2;
                if k < bytes.len() {
                    k += 1;
                    while k < bytes.len() && k < i + 14 && bytes[k] != b'\'' {
                        k += 1;
                    }
                    if k < bytes.len() && bytes[k] == b'\'' {
                        end = Some(k + 1);
                    }
                }
            } else if let Some(ch) = source.get(i + 1..).and_then(|s| s.chars().next()) {
                let next_idx = i + 1 + ch.len_utf8();
                if bytes.get(next_idx) == Some(&b'\'') {
                    end = Some(next_idx + 1);
                }
            }
            if let Some(next_i) = end {
                i = next_i;
            } else {
                result.push(Token {
                    value: "'",
                    literal: false,
                    offset: i,
                    end: i + 1,
                });
                i += 1;
            }
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
                end: i,
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
                end: i,
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
                end: i,
            });
        } else {
            let len = source.get(i..).unwrap().chars().next().unwrap().len_utf8();
            result.push(Token {
                value: source.get(i..i + len).unwrap(),
                literal: false,
                offset: i,
                end: i + len,
            });
            i += len;
        }
    }
    result
}

/// Replace `#[cfg(test)]`-annotated items, modules, fields, and statements with spaces while
/// keeping every newline intact so reported line numbers stay exact.
fn strip_cfg_test(source: &str) -> String {
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Kind {
        SemicolonOnly,
        ItemWithBodyOrSemi,
        IfExpr,
        BlockExpr,
        FieldOrStmt,
    }

    let code = tokens(source);
    let mut masked = source.as_bytes().to_vec();
    let mut i = 0;

    let is_attr = |toks: &[Token<'_>], pos: usize| -> Option<usize> {
        if pos + 2 >= toks.len()
            || toks[pos].literal
            || toks[pos].value != "#"
            || toks[pos + 1].literal
            || toks[pos + 1].value != "["
        {
            return None;
        }
        let mut depth = 0usize;
        let mut j = pos + 1;
        while j < toks.len() {
            if !toks[j].literal {
                match toks[j].value {
                    "[" => depth += 1,
                    "]" => {
                        depth = depth.saturating_sub(1);
                        if depth == 0 {
                            return Some(j + 1);
                        }
                    }
                    _ => {}
                }
            }
            j += 1;
        }
        Some(toks.len())
    };

    // Match `#[cfg(test)]` and `#[cfg(all(..., test, ...))]`, returning the token index
    // immediately after the attribute. `#[cfg(any(test, ...))]` and `#[cfg(not(test))]`
    // are intentionally NOT stripped because they compile into production builds.
    let cfg_test_end = |toks: &[Token<'_>], pos: usize| -> Option<usize> {
        let attr_end = is_attr(toks, pos)?;
        if attr_end < pos + 7
            || toks[pos + 2].literal
            || toks[pos + 2].value != "cfg"
            || toks[pos + 3].literal
            || toks[pos + 3].value != "("
            || toks[attr_end - 2].literal
            || toks[attr_end - 2].value != ")"
        {
            return None;
        }
        let inner = &toks[pos + 4..attr_end - 2];
        if inner.len() == 1 && !inner[0].literal && inner[0].value == "test" {
            return Some(attr_end);
        }
        if inner.len() >= 4
            && !inner[0].literal
            && inner[0].value == "all"
            && !inner[1].literal
            && inner[1].value == "("
            && !inner[inner.len() - 1].literal
            && inner[inner.len() - 1].value == ")"
        {
            let args = &inner[2..inner.len() - 1];
            let mut depth = 0usize;
            for (idx, tok) in args.iter().enumerate() {
                if !tok.literal {
                    match tok.value {
                        "(" => depth += 1,
                        ")" => depth = depth.saturating_sub(1),
                        "test" if depth == 0 => {
                            let prev_ok =
                                idx == 0 || (!args[idx - 1].literal && args[idx - 1].value == ",");
                            let next_ok = idx + 1 == args.len()
                                || (!args[idx + 1].literal && args[idx + 1].value == ",");
                            if prev_ok && next_ok {
                                return Some(attr_end);
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
        None
    };

    while i < code.len() {
        let Some(mut j) = cfg_test_end(&code, i) else {
            i += 1;
            continue;
        };
        let span_start = code[i].offset;
        // Skip any stacked attributes following `#[cfg(test)]`.
        while let Some(next) = is_attr(&code, j) {
            j = next;
        }
        if j >= code.len() {
            for b in &mut masked[span_start..] {
                if *b != b'\n' {
                    *b = b' ';
                }
            }
            break;
        }

        // Skip leading visibility and item qualifiers to classify the construct.
        let mut head = j;
        while head < code.len() && !code[head].literal {
            match code[head].value {
                "pub" => {
                    head += 1;
                    if head < code.len() && !code[head].literal && code[head].value == "(" {
                        let mut d = 0usize;
                        while head < code.len() {
                            if !code[head].literal {
                                if code[head].value == "(" {
                                    d += 1;
                                } else if code[head].value == ")" {
                                    d = d.saturating_sub(1);
                                    head += 1;
                                    if d == 0 {
                                        break;
                                    }
                                    continue;
                                }
                            }
                            head += 1;
                        }
                    }
                }
                "async" | "unsafe" | "default" => head += 1,
                "extern" => {
                    head += 1;
                    if head < code.len() && code[head].literal {
                        head += 1;
                    }
                }
                "const"
                    if code
                        .get(head + 1)
                        .is_some_and(|t| !t.literal && matches!(t.value, "fn" | "unsafe")) =>
                {
                    head += 1;
                }
                _ => break,
            }
        }

        let kind = match code.get(head).filter(|t| !t.literal).map(|t| t.value) {
            Some("use" | "const" | "static" | "type" | "let") => Kind::SemicolonOnly,
            Some("fn" | "mod" | "impl" | "trait" | "struct" | "enum" | "union" | "macro_rules") => {
                Kind::ItemWithBodyOrSemi
            }
            Some("if") => Kind::IfExpr,
            Some("for" | "while" | "loop" | "match" | "{") => Kind::BlockExpr,
            _ => Kind::FieldOrStmt,
        };

        let mut paren = 0usize;
        let mut bracket = 0usize;
        let mut brace = 0usize;
        let mut angle = 0usize;
        let mut end_tok = j;

        while end_tok < code.len() {
            let tok = &code[end_tok];
            if !tok.literal {
                match tok.value {
                    "(" => paren += 1,
                    ")" => {
                        if paren == 0 && bracket == 0 && brace == 0 {
                            break;
                        }
                        paren -= 1;
                    }
                    "[" => bracket += 1,
                    "]" => {
                        if paren == 0 && bracket == 0 && brace == 0 {
                            break;
                        }
                        bracket -= 1;
                    }
                    "{" => brace += 1,
                    "}" => {
                        if brace == 0 {
                            // Closing delimiter of an enclosing struct/block (e.g., last field without comma).
                            break;
                        }
                        brace -= 1;
                        if paren == 0 && bracket == 0 && brace == 0 {
                            match kind {
                                Kind::ItemWithBodyOrSemi => {
                                    end_tok += 1;
                                    break;
                                }
                                Kind::BlockExpr => {
                                    end_tok += 1;
                                    if code
                                        .get(end_tok)
                                        .is_some_and(|t| !t.literal && t.value == ";")
                                    {
                                        end_tok += 1;
                                    }
                                    break;
                                }
                                Kind::IfExpr => {
                                    end_tok += 1;
                                    if code
                                        .get(end_tok)
                                        .is_some_and(|t| !t.literal && t.value == "else")
                                    {
                                        end_tok += 1;
                                        continue;
                                    }
                                    if code
                                        .get(end_tok)
                                        .is_some_and(|t| !t.literal && t.value == ";")
                                    {
                                        end_tok += 1;
                                    }
                                    break;
                                }
                                Kind::SemicolonOnly | Kind::FieldOrStmt => {}
                            }
                        }
                    }
                    "<" if kind == Kind::FieldOrStmt
                        && paren == 0
                        && bracket == 0
                        && brace == 0 =>
                    {
                        let next_is_type = code.get(end_tok + 1).is_some_and(|t| {
                            !t.literal
                                && (t.value.chars().next().is_some_and(char::is_alphabetic)
                                    || matches!(t.value, "_" | "&" | "[" | "("))
                        });
                        if next_is_type {
                            angle += 1;
                        }
                    }
                    ">" if kind == Kind::FieldOrStmt
                        && paren == 0
                        && bracket == 0
                        && brace == 0
                        && angle > 0 =>
                    {
                        let prev = end_tok.checked_sub(1).and_then(|p| code.get(p));
                        let is_arrow = prev.is_some_and(|p| {
                            !p.literal && matches!(p.value, "-" | "=") && p.end == tok.offset
                        });
                        if !is_arrow {
                            angle -= 1;
                        }
                    }
                    ";" if paren == 0 && bracket == 0 && brace == 0 => {
                        end_tok += 1;
                        break;
                    }
                    "," if kind == Kind::FieldOrStmt
                        && paren == 0
                        && bracket == 0
                        && brace == 0
                        && angle == 0 =>
                    {
                        end_tok += 1;
                        break;
                    }
                    _ => {}
                }
            }
            end_tok += 1;
        }

        let span_end = if end_tok > i {
            code[end_tok - 1].end
        } else {
            code[i].end
        };
        for b in &mut masked[span_start..span_end] {
            if *b != b'\n' {
                *b = b' ';
            }
        }
        i = end_tok;
    }

    String::from_utf8(masked).unwrap()
}

fn violations(source: &str) -> Vec<usize> {
    let code = tokens(source);
    let mut violations = Vec::new();
    for (i, token) in code.iter().enumerate().filter(|(_, t)| t.literal) {
        let previous: Vec<_> = code[..i].iter().rev().take(8).map(|t| t.value).collect();
        if previous.starts_with(&["=", "reason"]) {
            continue;
        }
        let hangul = has_hangul(token.value);
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
            || previous.starts_with(&["(", "os_action", ":", ":", "MenuItem"])
            || previous.starts_with(&["(", "new", ":", ":", "Menu"])
            || previous.starts_with(&["(", "new", ":", ":", "PopupMenuItem"])
            || previous.starts_with(&["(", "new", ":", ":", "Tooltip"])
            || previous.starts_with(&["(", "!", "format", "(", "new", ":", ":", "PopupMenuItem"])
            || previous.starts_with(&["(", "!", "format", "(", "new", ":", ":", "Tooltip"]);
        let ui_helper = previous.starts_with(&["(", "field_label", ":", ":"])
            || (previous.first() == Some(&",")
                && previous.get(2) == Some(&"(")
                && previous.get(3).is_some_and(|name| {
                    matches!(*name, "primary" | "secondary" | "ghost" | "error_banner")
                })
                && previous.get(4) == Some(&":")
                && previous.get(5) == Some(&":")
                && previous
                    .get(6)
                    .is_some_and(|ns| matches!(*ns, "ui" | "layout")));
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
            || ((direct_ui || formatted_ui || menu || ui_helper)
                && visible.chars().any(char::is_alphabetic))
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
            let production = strip_cfg_test(&source);
            for line in violations(&production) {
                failures.push(format!("{}:{line}", path.display()));
            }
        }
    }
}

/// CLI와 데몬 알림은 언어 설정을 모르므로 영어로 쓴다. 앱에 보이는 데몬 알림은 앱이 카탈로그로 번역한다.
#[test]
fn cli_and_daemon_messages_are_english() {
    fn scan(path: &Path, failures: &mut Vec<String>) {
        if path.is_dir() {
            for entry in fs::read_dir(path).unwrap() {
                scan(&entry.unwrap().path(), failures);
            }
        } else if path.extension().is_some_and(|ext| ext == "rs")
            && !path.to_string_lossy().contains("tests")
        {
            let source = fs::read_to_string(path).unwrap();
            // 처음 나오는 테스트 모듈 앞까지가 제품 코드다. `#[cfg(test)]` 항목은 파일 중간에도 있어 기준으로 삼지 않는다.
            let end = source
                .match_indices("\nmod ")
                .find(|(at, _)| {
                    source
                        .get(at + 1..)
                        .and_then(|rest| rest.lines().next())
                        .is_some_and(|line| line.contains("tests"))
                })
                .map_or(source.len(), |(at, _)| at);
            let production = source.get(..end).unwrap();
            let code = tokens(production);
            for (i, token) in code.iter().enumerate().filter(|(_, t)| t.literal) {
                let previous: Vec<_> = code[..i].iter().rev().take(2).map(|t| t.value).collect();
                if has_hangul(token.value) && !previous.starts_with(&["=", "reason"]) {
                    let line = production.get(..token.offset).unwrap().lines().count();
                    failures.push(format!("{}:{line}", path.display()));
                }
            }
        }
    }
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let mut failures = Vec::new();
    for path in [
        "krill/src",
        "krill-link/src",
        "krill-link-core/src",
        "krill-proto/src/statusline.rs",
        "krill-proto/src/usage/connections.rs",
        "krilld/src/usage/direct.rs",
        "krilld/src/mobile/gateway.rs",
        "krilld/src/mobile/relay.rs",
    ] {
        scan(&crates.join(path), &mut failures);
    }
    assert!(
        failures.is_empty(),
        "Korean message literals:\n{}",
        failures.join("\n")
    );
}

#[test]
fn app_ui_copy_uses_the_catalog() {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let mut failures = Vec::new();
    for crate_name in ["krill-ui", "krill-app-core", "krill-app"] {
        check_dir(&crates.join(crate_name).join("src"), &mut failures);
    }
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
        r#"div().child("ㄱㄴ")"#,
        r#"div().child("\u{1100}")"#,
        r#"Button::new("id").label("Settings")"#,
        r##"input.placeholder(r#"Search"#)"##,
        r#"div().child(format!("Hello {name}"))"#,
        r#"MenuItem::action("Close", action)"#,
        r#"MenuItem::os_action("Copy", Copy, OsAction::Copy)"#,
        r#"PopupMenuItem::new("Rename")"#,
        r#"Tooltip::new("Hint")"#,
        r#"ui::primary("save-btn", "Save")"#,
        r#"layout::secondary("copy-btn", "Copy")"#,
        r#"ui::error_banner(p, "Failed")"#,
        r#"ui::field_label("Name", None, p)"#,
        r#"let x = "여러\n줄";"#,
    ] {
        assert!(!violations(source).is_empty(), "{source}");
    }
    assert!(violations("// .label(\"설정\")\n/* nested /* 설정 */ comment */\n#[expect(lint, reason = \"근거\")] fn f() {}\nlet quote = '\"'; let escaped = '\\''; let q = \"''\"; // 한글 주석\ndiv().id(\"settings\").child(text::TITLE.get());").is_empty());
}

#[test]
fn strip_cfg_test_keeps_production_code_after_mid_file_test_items() {
    let sample = r#"
struct View {
    #[cfg(test)]
    pub(super) map: std::collections::HashMap<String, usize>,
    real: usize,
}

#[cfg(test)]
#[cfg(unix)]
fn helper() {
    let _ = "테스트 한글";
}

#[cfg(all(test, unix))]
mod unix_tests {
    fn ok() {
        let _ = "유닉스 테스트 한글";
    }
}

#[cfg(any(test, windows))]
fn prod_on_windows() {
    let _ = div().child("WindowsOnly");
}

fn prod() {
    #[cfg(test)]
    if true {
        let _ = "테스트";
    } else {
        let _ = "테스트2";
    }
    let _ = div().child("Hardcoded");
}

#[cfg(test)]
mod tests {
    #[test]
    fn ok() {
        let _ = "한글 허용";
    }
}
"#;
    let stripped = strip_cfg_test(sample);
    assert_eq!(stripped.lines().count(), sample.lines().count());
    let lines = violations(&stripped);
    assert_eq!(lines, vec![23, 33]);
}
