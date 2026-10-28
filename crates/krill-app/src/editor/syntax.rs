//! 내장 경량 문법 하이라이터(HTML·CSS·JavaScript/TypeScript·TOML·YAML·Bash·SQL·Diff).
//!
//! `gpui-component`가 기본 탑재한 `rust`·`json` 외에 외부 tree-sitter C 문법 크레이트 의존성 없이
//! 파일 에디터에서 바로 문법 색상과 코드 접기(folding)를 제공한다.

use std::ops::Range;
use std::rc::Rc;

use gpui::{Context, HighlightStyle, SharedString, Window};
use gpui_component::input::{
    EditorState, FoldRange, HighlightStyleResolver, InputEdit, InputHighlighter,
    InputHighlighterFactory, Rope,
};

/// 파일 에디터(`EditorState`)에 경량 문법 하이라이터 팩토리를 설치한다.
/// 지원하는 언어(`html`, `css`, `javascript`, `typescript`, `tsx`, `toml`, `yaml`, `bash`, `sql`, `diff`)는
/// 내장 토크나이저를 쓰고, 그 외(`rust`, `json` 등)는 `gpui-component` 기본 하이라이터를 그대로 유지한다.
pub fn install(state: &mut EditorState, language: &str, cx: &mut Context<EditorState>) {
    if supports(language) {
        state.set_highlighter_factory(factory(), cx);
    }
}

/// 경량 문법 하이라이터를 지원하는 언어인가.
#[must_use]
pub fn supports(language: &str) -> bool {
    Lang::parse(language).is_some()
}

/// HTML 문서 본문에서 `<title>...</title>` 텍스트를 추출한다.
#[must_use]
pub fn html_title(src: &str) -> Option<String> {
    let bytes = src.as_bytes();
    let open = find_ascii_case_insensitive(bytes, b"<title", 0)?;
    let after_name = open + b"<title".len();
    if bytes
        .get(after_name)
        .is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'-' || *b == b'_')
    {
        return None;
    }
    let tag_close = bytes[after_name..].iter().position(|&b| b == b'>')? + after_name + 1;
    let close = find_ascii_case_insensitive(bytes, b"</title>", tag_close)?;
    let raw = src.get(tag_close..close)?.trim();
    if raw.is_empty() {
        return None;
    }
    let collapsed: String = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        None
    } else {
        Some(collapsed.chars().take(160).collect())
    }
}

/// 지정한 언어로 소스를 토크나이즈한 `(바이트 범위, 하이라이트 종류)` 목록을 돌려준다.
#[must_use]
pub fn tokenize_for(language: &str, src: &str) -> Vec<(Range<usize>, &'static str)> {
    let Some(lang) = Lang::parse(language) else {
        return Vec::new();
    };
    tokenize(lang, src)
        .into_iter()
        .map(|span| (span.range, span.kind))
        .collect()
}

fn factory() -> InputHighlighterFactory {
    Rc::new(|language| {
        Lang::parse(language).map(|lang| {
            Box::new(BuiltinHighlighter {
                lang,
                name: SharedString::from(language.to_owned()),
                text: String::new(),
                tokens: Vec::new(),
            }) as Box<dyn InputHighlighter>
        })
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Lang {
    Html,
    Css,
    Js,
    Toml,
    Yaml,
    Bash,
    Sql,
    Diff,
}

impl Lang {
    fn parse(name: &str) -> Option<Self> {
        match name {
            "html" | "htm" | "svg" | "xml" => Some(Self::Html),
            "css" | "scss" => Some(Self::Css),
            "javascript" | "js" | "jsx" | "typescript" | "ts" | "tsx" => Some(Self::Js),
            "toml" => Some(Self::Toml),
            "yaml" | "yml" => Some(Self::Yaml),
            "bash" | "sh" | "zsh" => Some(Self::Bash),
            "sql" => Some(Self::Sql),
            "diff" | "patch" => Some(Self::Diff),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct TokenSpan {
    range: Range<usize>,
    kind: &'static str,
}

struct BuiltinHighlighter {
    lang: Lang,
    name: SharedString,
    text: String,
    tokens: Vec<TokenSpan>,
}

impl InputHighlighter for BuiltinHighlighter {
    fn language(&self) -> SharedString {
        self.name.clone()
    }

    fn update(
        &mut self,
        _edit: Option<InputEdit>,
        text: &Rope,
        _folding: bool,
        _window: &mut Window,
        _cx: &mut Context<EditorState>,
    ) {
        self.text = text.to_string();
        self.tokens = tokenize(self.lang, &self.text);
    }

    fn styles(
        &self,
        range: &Range<usize>,
        resolver: &dyn HighlightStyleResolver,
    ) -> Vec<(Range<usize>, HighlightStyle)> {
        if range.is_empty() {
            return Vec::new();
        }
        let start_idx = self.tokens.partition_point(|t| t.range.end <= range.start);
        let mut out = Vec::new();
        let mut cursor = range.start;
        for token in self.tokens.get(start_idx..).unwrap_or_default() {
            if token.range.start >= range.end {
                break;
            }
            let s = token.range.start.max(range.start);
            let e = token.range.end.min(range.end);
            if s > cursor {
                out.push((cursor..s, HighlightStyle::default()));
            }
            if s < e {
                let style = resolver.style(token.kind).unwrap_or_default();
                out.push((s..e, style));
                cursor = e;
            }
        }
        if cursor < range.end {
            out.push((cursor..range.end, HighlightStyle::default()));
        }
        out
    }

    fn fold_ranges(&self, _text: &Rope) -> Vec<FoldRange> {
        Vec::new()
    }
}

fn tokenize(lang: Lang, src: &str) -> Vec<TokenSpan> {
    let mut out = Vec::new();
    match lang {
        Lang::Html => tokenize_html(src, 0, &mut out),
        Lang::Css => tokenize_css(src, 0, &mut out),
        Lang::Js => tokenize_js(src, 0, &mut out),
        Lang::Toml => tokenize_toml(src, 0, &mut out),
        Lang::Yaml => tokenize_yaml(src, 0, &mut out),
        Lang::Bash => tokenize_bash(src, 0, &mut out),
        Lang::Sql => tokenize_sql(src, 0, &mut out),
        Lang::Diff => tokenize_diff(src, 0, &mut out),
    }
    out
}

fn push(out: &mut Vec<TokenSpan>, base: usize, range: Range<usize>, kind: &'static str) {
    if range.start < range.end {
        out.push(TokenSpan {
            range: (base + range.start)..(base + range.end),
            kind,
        });
    }
}

fn is_ident_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_' || b == b'$'
}

fn is_ident_continue(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'$' || b == b'-'
}

fn find_ascii_case_insensitive(haystack: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if needle.is_empty() || from + needle.len() > haystack.len() {
        return None;
    }
    haystack[from..]
        .windows(needle.len())
        .position(|w| w.eq_ignore_ascii_case(needle))
        .map(|pos| from + pos)
}

fn tokenize_html(src: &str, base: usize, out: &mut Vec<TokenSpan>) {
    let bytes = src.as_bytes();
    let len = bytes.len();
    let mut i = 0;

    while i < len {
        if bytes[i] != b'<' {
            i += 1;
            continue;
        }
        // HTML comment <!-- ... -->
        if bytes[i..].starts_with(b"<!--") {
            let end = src
                .get(i + 4..)
                .unwrap_or_default()
                .find("-->")
                .map_or(len, |pos| i + 4 + pos + 3);
            push(out, base, i..end, "comment");
            i = end;
            continue;
        }
        // DOCTYPE <!doctype ...>
        if bytes[i..].len() >= 9 && bytes[i..i + 9].eq_ignore_ascii_case(b"<!doctype") {
            let end = bytes[i..]
                .iter()
                .position(|&b| b == b'>')
                .map_or(len, |p| i + p + 1);
            push(out, base, i..end, "keyword");
            i = end;
            continue;
        }
        // Closing tag </tag>
        if bytes.get(i + 1) == Some(&b'/') {
            push(out, base, i..(i + 2), "punctuation.bracket");
            i += 2;
            let name_start = i;
            while i < len && (is_ident_continue(bytes[i]) || bytes[i] == b':') {
                i += 1;
            }
            push(out, base, name_start..i, "tag");
            while i < len && bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            if bytes.get(i) == Some(&b'>') {
                push(out, base, i..(i + 1), "punctuation.bracket");
                i += 1;
            }
            continue;
        }
        // Opening tag <tag ...>
        if bytes.get(i + 1).is_some_and(|&b| is_ident_start(b)) {
            push(out, base, i..(i + 1), "punctuation.bracket");
            i += 1;
            let name_start = i;
            while i < len && (is_ident_continue(bytes[i]) || bytes[i] == b':') {
                i += 1;
            }
            let tag_name = src.get(name_start..i).unwrap_or_default();
            push(out, base, name_start..i, "tag");

            // Attributes inside tag
            while i < len {
                while i < len && bytes[i].is_ascii_whitespace() {
                    i += 1;
                }
                if i >= len {
                    break;
                }
                if bytes[i..].starts_with(b"/>") {
                    push(out, base, i..(i + 2), "punctuation.bracket");
                    i += 2;
                    break;
                }
                if bytes[i] == b'>' {
                    push(out, base, i..(i + 1), "punctuation.bracket");
                    i += 1;
                    // Embedded <style> or <script>
                    if tag_name.eq_ignore_ascii_case("style") {
                        let close =
                            find_ascii_case_insensitive(bytes, b"</style", i).unwrap_or(len);
                        tokenize_css(src.get(i..close).unwrap_or_default(), base + i, out);
                        i = close;
                    } else if tag_name.eq_ignore_ascii_case("script") {
                        let close =
                            find_ascii_case_insensitive(bytes, b"</script", i).unwrap_or(len);
                        tokenize_js(src.get(i..close).unwrap_or_default(), base + i, out);
                        i = close;
                    }
                    break;
                }
                // Attribute name
                let attr_start = i;
                while i < len
                    && !bytes[i].is_ascii_whitespace()
                    && !matches!(bytes[i], b'=' | b'>' | b'/' | b'"' | b'\'')
                {
                    i += 1;
                }
                if i > attr_start {
                    push(out, base, attr_start..i, "attribute");
                } else {
                    i += 1;
                    continue;
                }
                while i < len && bytes[i].is_ascii_whitespace() {
                    i += 1;
                }
                if bytes.get(i) == Some(&b'=') {
                    push(out, base, i..(i + 1), "operator");
                    i += 1;
                    while i < len && bytes[i].is_ascii_whitespace() {
                        i += 1;
                    }
                    if let Some(&quote @ (b'"' | b'\'')) = bytes.get(i) {
                        let val_start = i;
                        i += 1;
                        while i < len && bytes[i] != quote {
                            i += 1;
                        }
                        if i < len {
                            i += 1;
                        }
                        push(out, base, val_start..i, "string");
                    } else {
                        let val_start = i;
                        while i < len
                            && !bytes[i].is_ascii_whitespace()
                            && !matches!(bytes[i], b'>' | b'/')
                        {
                            i += 1;
                        }
                        push(out, base, val_start..i, "string");
                    }
                }
            }
            continue;
        }
        i += 1;
    }
}

fn tokenize_css(src: &str, base: usize, out: &mut Vec<TokenSpan>) {
    let bytes = src.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    let mut brace_depth: usize = 0;
    let mut after_colon = false;

    while i < len {
        let b = bytes[i];
        if b.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        // Block comment /* ... */
        if bytes[i..].starts_with(b"/*") {
            let end = src
                .get(i + 2..)
                .unwrap_or_default()
                .find("*/")
                .map_or(len, |pos| i + 2 + pos + 2);
            push(out, base, i..end, "comment");
            i = end;
            continue;
        }
        // Quoted string
        if b == b'"' || b == b'\'' {
            let end = scan_quoted(bytes, i);
            push(out, base, i..end, "string");
            i = end;
            continue;
        }
        if b == b'{' {
            brace_depth += 1;
            after_colon = false;
            push(out, base, i..(i + 1), "punctuation.bracket");
            i += 1;
            continue;
        }
        if b == b'}' {
            brace_depth = brace_depth.saturating_sub(1);
            after_colon = false;
            push(out, base, i..(i + 1), "punctuation.bracket");
            i += 1;
            continue;
        }
        if b == b';' {
            after_colon = false;
            push(out, base, i..(i + 1), "punctuation.delimiter");
            i += 1;
            continue;
        }
        if b == b':' && brace_depth > 0 && !after_colon {
            after_colon = true;
            push(out, base, i..(i + 1), "punctuation.delimiter");
            i += 1;
            continue;
        }
        // At-rule (@media, @keyframes, @import)
        if b == b'@' {
            let start = i;
            i += 1;
            while i < len && is_ident_continue(bytes[i]) {
                i += 1;
            }
            push(out, base, start..i, "keyword");
            continue;
        }
        // Hex color or ID selector
        if b == b'#' {
            let start = i;
            i += 1;
            while i < len && is_ident_continue(bytes[i]) {
                i += 1;
            }
            push(
                out,
                base,
                start..i,
                if after_colon { "constant" } else { "type" },
            );
            continue;
        }
        // Numbers & units (e.g. 16px, 1.5rem, 100%)
        if b.is_ascii_digit() || (b == b'.' && bytes.get(i + 1).is_some_and(u8::is_ascii_digit)) {
            let start = i;
            i += 1;
            while i < len && (bytes[i].is_ascii_alphanumeric() || matches!(bytes[i], b'.' | b'%')) {
                i += 1;
            }
            push(out, base, start..i, "number");
            continue;
        }
        // Custom property `--var` or property/selector identifier
        if is_ident_start(b) || b == b'-' || b == b'.' {
            let start = i;
            i += 1;
            while i < len && is_ident_continue(bytes[i]) {
                i += 1;
            }
            let word = src.get(start..i).unwrap_or_default();
            if word.starts_with("--") {
                push(out, base, start..i, "variable");
            } else if bytes.get(i) == Some(&b'(') {
                push(out, base, start..i, "function");
            } else if brace_depth > 0 && !after_colon {
                push(out, base, start..i, "property");
            } else if brace_depth == 0 {
                push(
                    out,
                    base,
                    start..i,
                    if word.starts_with('.') { "type" } else { "tag" },
                );
            } else if matches!(
                word,
                "important"
                    | "none"
                    | "auto"
                    | "inherit"
                    | "initial"
                    | "unset"
                    | "solid"
                    | "flex"
                    | "grid"
                    | "block"
                    | "inline"
                    | "inline-block"
                    | "relative"
                    | "absolute"
                    | "fixed"
                    | "sticky"
                    | "hidden"
                    | "scroll"
                    | "pointer"
                    | "center"
                    | "transparent"
            ) {
                push(out, base, start..i, "constant");
            }
            continue;
        }
        if matches!(b, b'(' | b')' | b'[' | b']') {
            push(out, base, i..(i + 1), "punctuation.bracket");
        }
        i += 1;
    }
}

fn tokenize_js(src: &str, base: usize, out: &mut Vec<TokenSpan>) {
    const KEYWORDS: &[&str] = &[
        "async",
        "await",
        "break",
        "case",
        "catch",
        "class",
        "const",
        "continue",
        "debugger",
        "default",
        "delete",
        "do",
        "else",
        "export",
        "extends",
        "finally",
        "for",
        "from",
        "function",
        "if",
        "import",
        "in",
        "instanceof",
        "interface",
        "let",
        "new",
        "of",
        "return",
        "static",
        "switch",
        "throw",
        "try",
        "type",
        "typeof",
        "var",
        "void",
        "while",
        "yield",
        "as",
        "implements",
        "enum",
        "public",
        "private",
        "protected",
        "readonly",
    ];
    const CONSTANTS: &[&str] = &[
        "true",
        "false",
        "null",
        "undefined",
        "NaN",
        "Infinity",
        "this",
        "super",
    ];

    let bytes = src.as_bytes();
    let len = bytes.len();
    let mut i = 0;

    while i < len {
        let b = bytes[i];
        if b.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        if bytes[i..].starts_with(b"//") {
            let end = bytes[i..]
                .iter()
                .position(|&c| c == b'\n')
                .map_or(len, |p| i + p);
            push(out, base, i..end, "comment");
            i = end;
            continue;
        }
        if bytes[i..].starts_with(b"/*") {
            let end = src
                .get(i + 2..)
                .unwrap_or_default()
                .find("*/")
                .map_or(len, |pos| i + 2 + pos + 2);
            push(out, base, i..end, "comment");
            i = end;
            continue;
        }
        if matches!(b, b'"' | b'\'' | b'`') {
            let end = scan_quoted(bytes, i);
            push(out, base, i..end, "string");
            i = end;
            continue;
        }
        if b.is_ascii_digit() {
            let start = i;
            i += 1;
            while i < len && (bytes[i].is_ascii_alphanumeric() || matches!(bytes[i], b'.' | b'_')) {
                i += 1;
            }
            push(out, base, start..i, "number");
            continue;
        }
        if is_ident_start(b) {
            let start = i;
            i += 1;
            while i < len && (bytes[i].is_ascii_alphanumeric() || matches!(bytes[i], b'_' | b'$')) {
                i += 1;
            }
            let word = src.get(start..i).unwrap_or_default();
            let kind = if KEYWORDS.contains(&word) {
                Some("keyword")
            } else if CONSTANTS.contains(&word) {
                Some("constant")
            } else if bytes.get(i) == Some(&b'(') {
                Some("function")
            } else if word.as_bytes().first().is_some_and(u8::is_ascii_uppercase) {
                Some("type")
            } else {
                None
            };
            if let Some(kind) = kind {
                push(out, base, start..i, kind);
            }
            continue;
        }
        if matches!(b, b'{' | b'}' | b'(' | b')' | b'[' | b']') {
            push(out, base, i..(i + 1), "punctuation.bracket");
        } else if matches!(
            b,
            b'=' | b'+' | b'-' | b'*' | b'!' | b'<' | b'>' | b'&' | b'|' | b'?'
        ) {
            push(out, base, i..(i + 1), "operator");
        }
        i += 1;
    }
}

fn tokenize_toml(src: &str, base: usize, out: &mut Vec<TokenSpan>) {
    let mut offset = 0;
    for line in src.split_inclusive('\n') {
        let bytes = line.as_bytes();
        let trimmed = line.trim_start();
        let lead = line.len() - trimmed.len();
        if trimmed.starts_with('#') {
            push(out, base + offset, lead..line.len(), "comment");
            offset += line.len();
            continue;
        }
        if trimmed.starts_with('[') {
            let end = trimmed.find('#').unwrap_or(trimmed.len());
            push(out, base + offset, lead..(lead + end), "type");
            if end < trimmed.len() {
                push(out, base + offset, (lead + end)..line.len(), "comment");
            }
            offset += line.len();
            continue;
        }
        let mut i = lead;
        if let Some(eq_rel) = bytes[lead..].iter().position(|&b| b == b'=') {
            let eq = lead + eq_rel;
            push(out, base + offset, lead..eq, "property");
            push(out, base + offset, eq..(eq + 1), "operator");
            i = eq + 1;
        }
        while i < bytes.len() {
            let b = bytes[i];
            if b == b'#' {
                push(out, base + offset, i..bytes.len(), "comment");
                break;
            }
            if b == b'"' || b == b'\'' {
                let end = scan_quoted(bytes, i);
                push(out, base + offset, i..end, "string");
                i = end;
                continue;
            }
            if b.is_ascii_digit() {
                let start = i;
                while i < bytes.len()
                    && (bytes[i].is_ascii_alphanumeric()
                        || matches!(bytes[i], b'.' | b'_' | b'-' | b':'))
                {
                    i += 1;
                }
                push(out, base + offset, start..i, "number");
                continue;
            }
            if is_ident_start(b) {
                let start = i;
                while i < bytes.len() && is_ident_continue(bytes[i]) {
                    i += 1;
                }
                if matches!(line.get(start..i).unwrap_or_default(), "true" | "false") {
                    push(out, base + offset, start..i, "boolean");
                }
                continue;
            }
            i += 1;
        }
        offset += line.len();
    }
}

fn tokenize_yaml(src: &str, base: usize, out: &mut Vec<TokenSpan>) {
    let mut offset = 0;
    for line in src.split_inclusive('\n') {
        let bytes = line.as_bytes();
        let trimmed = line.trim_start();
        let lead = line.len() - trimmed.len();
        if trimmed.starts_with('#') {
            push(out, base + offset, lead..line.len(), "comment");
            offset += line.len();
            continue;
        }
        let mut i = lead;
        if trimmed.starts_with("- ") {
            push(
                out,
                base + offset,
                lead..(lead + 1),
                "punctuation.list_marker",
            );
            i += 2;
        }
        if let Some(colon_rel) = line.get(i..).unwrap_or_default().find(':') {
            let colon = i + colon_rel;
            if bytes.get(colon + 1).is_none_or(u8::is_ascii_whitespace) {
                push(out, base + offset, i..colon, "property");
                push(
                    out,
                    base + offset,
                    colon..(colon + 1),
                    "punctuation.delimiter",
                );
                i = colon + 1;
            }
        }
        while i < bytes.len() {
            let b = bytes[i];
            if b == b'#'
                && bytes
                    .get(i.saturating_sub(1))
                    .is_some_and(u8::is_ascii_whitespace)
            {
                push(out, base + offset, i..bytes.len(), "comment");
                break;
            }
            if b == b'"' || b == b'\'' {
                let end = scan_quoted(bytes, i);
                push(out, base + offset, i..end, "string");
                i = end;
                continue;
            }
            if b.is_ascii_digit() {
                let start = i;
                while i < bytes.len()
                    && (bytes[i].is_ascii_alphanumeric() || matches!(bytes[i], b'.' | b'-'))
                {
                    i += 1;
                }
                push(out, base + offset, start..i, "number");
                continue;
            }
            if is_ident_start(b) {
                let start = i;
                while i < bytes.len() && is_ident_continue(bytes[i]) {
                    i += 1;
                }
                if matches!(
                    line.get(start..i).unwrap_or_default(),
                    "true" | "false" | "null" | "yes" | "no"
                ) {
                    push(out, base + offset, start..i, "boolean");
                }
                continue;
            }
            i += 1;
        }
        offset += line.len();
    }
}

fn tokenize_bash(src: &str, base: usize, out: &mut Vec<TokenSpan>) {
    const KEYWORDS: &[&str] = &[
        "if", "then", "else", "elif", "fi", "for", "in", "do", "done", "while", "until", "case",
        "esac", "function", "return", "exit", "local", "export", "readonly", "set", "unset",
    ];
    let bytes = src.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    while i < len {
        let b = bytes[i];
        if b == b'#' && (i == 0 || bytes[i - 1].is_ascii_whitespace()) {
            let end = bytes[i..]
                .iter()
                .position(|&c| c == b'\n')
                .map_or(len, |p| i + p);
            push(out, base, i..end, "comment");
            i = end;
            continue;
        }
        if b == b'"' || b == b'\'' {
            let end = scan_quoted(bytes, i);
            push(out, base, i..end, "string");
            i = end;
            continue;
        }
        if b == b'$' {
            let start = i;
            i += 1;
            if bytes.get(i) == Some(&b'{') {
                while i < len && bytes[i] != b'}' {
                    i += 1;
                }
                if i < len {
                    i += 1;
                }
            } else {
                while i < len && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                    i += 1;
                }
            }
            push(out, base, start..i, "variable");
            continue;
        }
        if is_ident_start(b) {
            let start = i;
            while i < len && is_ident_continue(bytes[i]) {
                i += 1;
            }
            if KEYWORDS.contains(&src.get(start..i).unwrap_or_default()) {
                push(out, base, start..i, "keyword");
            }
            continue;
        }
        i += 1;
    }
}

fn tokenize_sql(src: &str, base: usize, out: &mut Vec<TokenSpan>) {
    const KEYWORDS: &[&str] = &[
        "SELECT",
        "FROM",
        "WHERE",
        "INSERT",
        "INTO",
        "VALUES",
        "UPDATE",
        "SET",
        "DELETE",
        "CREATE",
        "TABLE",
        "INDEX",
        "DROP",
        "ALTER",
        "ADD",
        "PRIMARY",
        "KEY",
        "FOREIGN",
        "REFERENCES",
        "JOIN",
        "LEFT",
        "RIGHT",
        "INNER",
        "OUTER",
        "ON",
        "AS",
        "AND",
        "OR",
        "NOT",
        "NULL",
        "IS",
        "IN",
        "EXISTS",
        "ORDER",
        "BY",
        "GROUP",
        "HAVING",
        "LIMIT",
        "OFFSET",
        "DISTINCT",
        "UNION",
        "ALL",
        "CASE",
        "WHEN",
        "THEN",
        "ELSE",
        "END",
        "BEGIN",
        "COMMIT",
        "ROLLBACK",
        "PRAGMA",
    ];
    let bytes = src.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    while i < len {
        let b = bytes[i];
        if bytes[i..].starts_with(b"--") {
            let end = bytes[i..]
                .iter()
                .position(|&c| c == b'\n')
                .map_or(len, |p| i + p);
            push(out, base, i..end, "comment");
            i = end;
            continue;
        }
        if b == b'\'' || b == b'"' {
            let end = scan_quoted(bytes, i);
            push(out, base, i..end, "string");
            i = end;
            continue;
        }
        if b.is_ascii_digit() {
            let start = i;
            while i < len && (bytes[i].is_ascii_digit() || bytes[i] == b'.') {
                i += 1;
            }
            push(out, base, start..i, "number");
            continue;
        }
        if is_ident_start(b) {
            let start = i;
            while i < len && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                i += 1;
            }
            let word = src.get(start..i).unwrap_or_default();
            if KEYWORDS.iter().any(|k| k.eq_ignore_ascii_case(word)) {
                push(out, base, start..i, "keyword");
            }
            continue;
        }
        i += 1;
    }
}

fn tokenize_diff(src: &str, base: usize, out: &mut Vec<TokenSpan>) {
    let mut offset = 0;
    for line in src.split_inclusive('\n') {
        let kind = if line.starts_with("@@") {
            Some("keyword")
        } else if line.starts_with("+++") || line.starts_with("---") {
            Some("title")
        } else if line.starts_with('+') {
            Some("string")
        } else if line.starts_with('-') {
            Some("constant")
        } else {
            None
        };
        if let Some(kind) = kind {
            push(out, base + offset, 0..line.len(), kind);
        }
        offset += line.len();
    }
}

fn scan_quoted(bytes: &[u8], start: usize) -> usize {
    let quote = bytes[start];
    let mut i = start + 1;
    while i < bytes.len() {
        if bytes[i] == b'\\' {
            i = (i + 2).min(bytes.len());
            continue;
        }
        if bytes[i] == quote {
            return i + 1;
        }
        i += 1;
    }
    bytes.len()
}
