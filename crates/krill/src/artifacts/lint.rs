//! 아티팩트 샌드박스·이식성·슬롭 정적 검사기(`krill artifacts lint`).
//!
//! `share.krill.page`의 불투명 Origin(`Origin: null`)에서 런타임 예외를 내는
//! 무방비 `localStorage`/`document.cookie` 호출, 업로드되지 않는 로컬 상대경로 에셋,
//! 뷰포트·제목 누락, 초대형 번들 남용, 대표적 슬롭 지문을 네트워크 없이 검사한다.

use std::path::Path;

use krill_proto::artifacts::{LintIssue, LintReport, LintSeverity};

use super::Error;

pub(super) fn lint_file(file: &str, strict: bool, as_json: bool) -> Result<String, Error> {
    let (key, body) = super::read_artifact(file, None)?;
    let ext = Path::new(&key)
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    let content = body["content"].as_str().unwrap_or_default();
    let report = lint_content(&ext, content);
    let failed = report.errors > 0 || (strict && report.warnings > 0);
    if failed {
        let summary = report
            .issues
            .iter()
            .filter(|i| strict || i.severity == LintSeverity::Error)
            .map(|i| format!("[{}] {}", i.code, i.message))
            .collect::<Vec<_>>()
            .join("; ");
        return Err(Error::coded("lint_failed", summary));
    }
    if as_json {
        return serde_json::to_string(&report).map_err(|e| Error::Fail(e.to_string()));
    }
    if report.issues.is_empty() {
        return Ok("ok".into());
    }
    let lines = report
        .issues
        .iter()
        .map(|i| format!("warning [{}]: {}", i.code, i.message))
        .collect::<Vec<_>>()
        .join("\n");
    Ok(lines)
}

pub(super) fn preflight_share(
    canonical_path: &str,
    content: &str,
    strict: bool,
) -> Result<Vec<String>, Error> {
    let ext = Path::new(canonical_path)
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    let report = lint_content(&ext, content);
    if strict && (report.errors > 0 || report.warnings > 0) {
        let summary = report
            .issues
            .iter()
            .map(|i| format!("[{}] {}", i.code, i.message))
            .collect::<Vec<_>>()
            .join("; ");
        return Err(Error::coded("lint_failed", summary));
    }
    Ok(report
        .issues
        .into_iter()
        .filter(|i| i.severity == LintSeverity::Error)
        .map(|i| format!("[{}] {}", i.code, i.message))
        .collect())
}

pub(super) fn lint_content(ext: &str, content: &str) -> LintReport {
    let mut issues = Vec::new();
    if matches!(ext, "html" | "htm") {
        lint_html(content, &mut issues);
    } else {
        lint_markdown(content, &mut issues);
    }
    let errors = issues
        .iter()
        .filter(|i| i.severity == LintSeverity::Error)
        .count();
    let warnings = issues
        .iter()
        .filter(|i| i.severity == LintSeverity::Warning)
        .count();
    LintReport {
        ok: errors == 0,
        errors,
        warnings,
        issues,
    }
}

fn lint_html(html: &str, issues: &mut Vec<LintIssue>) {
    let lower = html.to_ascii_lowercase();

    for script in extract_executable_scripts(html) {
        let cleaned = strip_js_comments(script);
        let has_storage = [
            "localStorage",
            "sessionStorage",
            "indexedDB",
            "document.cookie",
        ]
        .iter()
        .any(|token| contains_ident(&cleaned, token));
        if has_storage && !contains_ident(&cleaned, "try") {
            issues.push(LintIssue {
                code: "sandbox_storage".into(),
                severity: LintSeverity::Error,
                message: "unguarded storage/cookie API in <script>; share.krill.page runs in an opaque Origin: null sandbox where localStorage/sessionStorage/indexedDB/document.cookie throw SecurityError".into(),
            });
            break;
        }
    }

    let markup_only = strip_raw_blocks(html, "script");
    let markup_only = strip_raw_blocks(&markup_only, "style");
    let markup_only = strip_html_comments(&markup_only);
    if let Some(rel) = find_relative_html_asset(&markup_only) {
        issues.push(LintIssue {
            code: "relative_asset".into(),
            severity: LintSeverity::Error,
            message: format!(
                "relative asset path '{rel}' will not be uploaded with a single-file artifact; inline the asset or use a version-pinned https:// URL"
            ),
        });
    }

    if !(lower.contains("<meta") && lower.contains("viewport")) {
        issues.push(LintIssue {
            code: "missing_viewport".into(),
            severity: LintSeverity::Warning,
            message: "missing <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\"> for mobile layouts".into(),
        });
    }
    if !has_non_empty_tag(&lower, "title") {
        issues.push(LintIssue {
            code: "missing_title".into(),
            severity: LintSeverity::Warning,
            message: "missing or empty <title> element".into(),
        });
    }

    if lower.contains("mermaid") && lower.contains("<script") {
        let diagram_markers =
            lower.matches("class=\"mermaid\"").count() + lower.matches("class='mermaid'").count();
        if (1..=2).contains(&diagram_markers) {
            issues.push(LintIssue {
                code: "heavy_mermaid".into(),
                severity: LintSeverity::Warning,
                message: "mermaid.min.js (~729 KiB gzip) loaded for <= 2 diagrams; prefer lightweight inline <svg> using var(--ka-*) tokens".into(),
            });
        }
    }

    if lower.contains("#f4f1ea") && lower.contains("#d97757") {
        issues.push(LintIssue {
            code: "slop_cliche".into(),
            severity: LintSeverity::Warning,
            message: "detected default #F4F1EA cream + #D97757 terracotta palette tell; prefer KADS light-dark() tokens".into(),
        });
    }
    if lower.contains("@babel/standalone") || lower.contains("babel.min.js") {
        issues.push(LintIssue {
            code: "slop_cliche".into(),
            severity: LintSeverity::Warning,
            message: "in-browser Babel transpiler adds >800 KiB payload; prefer Preact + Hooks + HTM (7 KiB gzip) or native JS".into(),
        });
    }
    if markup_only.to_ascii_lowercase().contains("lorem ipsum") {
        issues.push(LintIssue {
            code: "slop_cliche".into(),
            severity: LintSeverity::Warning,
            message: "placeholder 'Lorem ipsum' text remains in document".into(),
        });
    }
}

fn lint_markdown(md: &str, issues: &mut Vec<LintIssue>) {
    let mut in_fence = false;
    let mut found_rel: Option<String> = None;
    let mut found_lorem = false;

    for line in md.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }
        if !found_lorem && line.to_ascii_lowercase().contains("lorem ipsum") {
            found_lorem = true;
        }
        if found_rel.is_none()
            && let Some(rel) = find_relative_markdown_image(line)
        {
            found_rel = Some(rel);
        }
    }

    if let Some(rel) = found_rel {
        issues.push(LintIssue {
            code: "relative_asset".into(),
            severity: LintSeverity::Error,
            message: format!(
                "relative image path '{rel}' does not travel with a shared Markdown file; use an https:// URL or a self-contained HTML artifact"
            ),
        });
    }
    if found_lorem {
        issues.push(LintIssue {
            code: "slop_cliche".into(),
            severity: LintSeverity::Warning,
            message: "placeholder 'Lorem ipsum' text remains in document".into(),
        });
    }
}

fn find_relative_markdown_image(line: &str) -> Option<String> {
    let mut rest = line;
    while let Some(pos) = rest.find("![") {
        let after_bang = rest.get(pos + 2..)?;
        let close_bracket = after_bang.find("](")?;
        let inside_parens = after_bang.get(close_bracket + 2..)?;
        let end_paren = inside_parens.find(')')?;
        let target_raw = inside_parens.get(..end_paren)?.trim();
        let url = target_raw
            .split_whitespace()
            .next()
            .unwrap_or_default()
            .trim_matches(['<', '>']);
        if !url.is_empty() && is_relative_asset_url(url) {
            return Some(url.to_owned());
        }
        rest = inside_parens.get(end_paren + 1..)?;
    }
    None
}

fn extract_executable_scripts(html: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let lower = html.to_ascii_lowercase();
    let mut offset = 0usize;
    while let Some(rel_start) = lower.get(offset..).and_then(|s| s.find("<script")) {
        let tag_start = offset + rel_start;
        let Some(after_open_rel) = lower.get(tag_start..).and_then(|s| s.find('>')) else {
            break;
        };
        let tag_end = tag_start + after_open_rel;
        let open_tag = lower.get(tag_start..=tag_end).unwrap_or_default();
        let body_start = tag_end + 1;
        let Some(close_rel) = lower.get(body_start..).and_then(|s| s.find("</script")) else {
            break;
        };
        let body_end = body_start + close_rel;
        let non_js_data = open_tag.contains("application/json")
            || open_tag.contains("application/ld+json")
            || open_tag.contains("text/plain")
            || open_tag.contains("text/markdown")
            || open_tag.contains("text/front-matter")
            || open_tag.contains("text/bibliography");
        if !non_js_data && let Some(body) = html.get(body_start..body_end) {
            out.push(body);
        }
        offset = body_end + "</script".len();
    }
    out
}

fn strip_js_comments(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    let mut chars = src.chars().peekable();
    let mut in_quote: Option<char> = None;
    while let Some(ch) = chars.next() {
        if let Some(q) = in_quote {
            out.push(ch);
            match ch {
                '\\' => {
                    if let Some(escaped) = chars.next() {
                        out.push(escaped);
                    }
                }
                c if c == q => in_quote = None,
                _ => {}
            }
            continue;
        }
        if matches!(ch, '\'' | '"' | '`') {
            in_quote = Some(ch);
            out.push(ch);
            continue;
        }
        if ch == '/' {
            if chars.peek() == Some(&'/') {
                chars.next();
                for c in chars.by_ref() {
                    if c == '\n' {
                        out.push('\n');
                        break;
                    }
                }
                continue;
            }
            if chars.peek() == Some(&'*') {
                chars.next();
                let mut prev = '\0';
                for c in chars.by_ref() {
                    if prev == '*' && c == '/' {
                        break;
                    }
                    prev = c;
                }
                out.push(' ');
                continue;
            }
        }
        out.push(ch);
    }
    out
}

fn contains_ident(haystack: &str, needle: &str) -> bool {
    let mut offset = 0usize;
    while let Some(pos) = haystack.get(offset..).and_then(|s| s.find(needle)) {
        let abs = offset + pos;
        let end = abs + needle.len();
        let before_ok = haystack
            .get(..abs)
            .and_then(|s| s.chars().next_back())
            .is_none_or(|c| !c.is_ascii_alphanumeric() && c != '_' && c != '$');
        let after_ok = haystack
            .get(end..)
            .and_then(|s| s.chars().next())
            .is_none_or(|c| !c.is_ascii_alphanumeric() && c != '_' && c != '$');
        if before_ok && after_ok {
            return true;
        }
        offset = abs + 1;
    }
    false
}

fn strip_raw_blocks(html: &str, tag: &str) -> String {
    let lower = html.to_ascii_lowercase();
    let open_pat = format!("<{tag}");
    let close_pat = format!("</{tag}");
    let mut out = String::with_capacity(html.len());
    let mut offset = 0usize;
    while let Some(rel_start) = lower.get(offset..).and_then(|s| s.find(&open_pat)) {
        let start = offset + rel_start;
        if let Some(prefix) = html.get(offset..start) {
            out.push_str(prefix);
        }
        let Some(open_end_rel) = lower.get(start..).and_then(|s| s.find('>')) else {
            offset = html.len();
            break;
        };
        let open_end = start + open_end_rel + 1;
        if let Some(open_slice) = html.get(start..open_end) {
            out.push_str(open_slice);
        }
        let Some(close_rel) = lower.get(open_end..).and_then(|s| s.find(&close_pat)) else {
            offset = html.len();
            break;
        };
        let close_start = open_end + close_rel;
        let close_end = lower
            .get(close_start..)
            .and_then(|s| s.find('>'))
            .map_or(html.len(), |r| close_start + r + 1);
        offset = close_end;
    }
    if let Some(tail) = html.get(offset..) {
        out.push_str(tail);
    }
    out
}

fn strip_html_comments(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut rest = html;
    while let Some(start) = rest.find("<!--") {
        if let Some(prefix) = rest.get(..start) {
            out.push_str(prefix);
        }
        let after = rest.get(start + 4..).unwrap_or_default();
        if let Some(end) = after.find("-->") {
            rest = after.get(end + 3..).unwrap_or_default();
        } else {
            return out;
        }
    }
    out.push_str(rest);
    out
}

fn find_relative_html_asset(markup: &str) -> Option<String> {
    let mut rest = markup;
    while let Some(lt) = rest.find('<') {
        let after_lt = rest.get(lt + 1..)?;
        let gt = after_lt.find('>')?;
        let tag_body = after_lt.get(..gt)?.trim();
        rest = after_lt.get(gt + 1..)?;
        if tag_body.starts_with(['/', '!', '?']) {
            continue;
        }
        let lower_tag = tag_body.to_ascii_lowercase();
        let tag_name = lower_tag
            .split(|c: char| c.is_ascii_whitespace() || c == '/')
            .next()
            .unwrap_or_default();
        if matches!(
            tag_name,
            "img" | "script" | "video" | "audio" | "source" | "iframe" | "embed"
        ) && let Some(val) = extract_attr(tag_body, "src")
            && is_relative_asset_url(&val)
        {
            return Some(val);
        }
        if tag_name == "link"
            && let Some(rel) = extract_attr(tag_body, "rel")
            && rel
                .split_ascii_whitespace()
                .any(|t| t.eq_ignore_ascii_case("stylesheet"))
            && let Some(href) = extract_attr(tag_body, "href")
            && is_relative_asset_url(&href)
        {
            return Some(href);
        }
    }
    None
}

fn extract_attr(tag_body: &str, attr_name: &str) -> Option<String> {
    let lower = tag_body.to_ascii_lowercase();
    let mut offset = 0usize;
    while let Some(pos) = lower.get(offset..).and_then(|s| s.find(attr_name)) {
        let abs = offset + pos;
        let before_ok = lower
            .get(..abs)
            .and_then(|s| s.chars().next_back())
            .is_some_and(|c| c.is_ascii_whitespace() || c == '"' || c == '\'');
        let after = tag_body
            .get(abs + attr_name.len()..)
            .unwrap_or_default()
            .trim_start();
        if before_ok && let Some(after_eq) = after.strip_prefix('=') {
            let trimmed = after_eq.trim_start();
            if let Some(quoted) = trimmed.strip_prefix('"') {
                let end = quoted.find('"')?;
                return quoted.get(..end).map(|s| s.trim().to_owned());
            }
            if let Some(quoted) = trimmed.strip_prefix('\'') {
                let end = quoted.find('\'')?;
                return quoted.get(..end).map(|s| s.trim().to_owned());
            }
            let end = trimmed
                .find(|c: char| c.is_ascii_whitespace() || c == '>')
                .unwrap_or(trimmed.len());
            return trimmed.get(..end).map(|s| s.trim().to_owned());
        }
        offset = abs + 1;
    }
    None
}

fn is_relative_asset_url(url: &str) -> bool {
    let u = url.trim();
    if u.is_empty() || u.starts_with("//") || u.starts_with('#') {
        return false;
    }
    let lower = u.to_ascii_lowercase();
    ![
        "https://",
        "http://",
        "data:",
        "blob:",
        "mailto:",
        "tel:",
        "javascript:",
        "about:",
    ]
    .iter()
    .any(|prefix| lower.starts_with(prefix))
}

fn has_non_empty_tag(lower_html: &str, tag: &str) -> bool {
    let open = format!("<{tag}");
    let close = format!("</{tag}>");
    let Some(start) = lower_html.find(&open) else {
        return false;
    };
    let Some(gt_rel) = lower_html.get(start..).and_then(|s| s.find('>')) else {
        return false;
    };
    let content_start = start + gt_rel + 1;
    let Some(end_rel) = lower_html.get(content_start..).and_then(|s| s.find(&close)) else {
        return false;
    };
    lower_html
        .get(content_start..content_start + end_rel)
        .is_some_and(|inner| !inner.trim().is_empty())
}
