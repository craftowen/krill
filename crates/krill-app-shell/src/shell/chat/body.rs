//! Fenced code in both completed messages and unfinished streaming output.

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::hash::{Hash as _, Hasher as _};
use std::sync::Arc;

use gpui::SharedString;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum ChatBodySegment {
    Text(SharedString),
    Code {
        lang: SharedString,
        code: SharedString,
    },
}

const MAX_SEGMENT_CACHE_ENTRIES: usize = 256;
const MAX_SEGMENT_CACHE_BYTES: usize = 64 * 1024;
const MAX_OVERSIZED_CACHE_ENTRIES: usize = 8;

type SegmentCacheKey = (u64, usize);
type SegmentCacheEntry = (String, Arc<[ChatBodySegment]>);

#[derive(Clone, PartialEq, Eq, Hash)]
struct OversizedSegmentKey {
    hash: u64,
    len: usize,
    head: [u8; 32],
    tail: [u8; 32],
}

impl OversizedSegmentKey {
    fn from_text(text: &str) -> Self {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        text.hash(&mut hasher);
        let bytes = text.as_bytes();
        let mut head = [0u8; 32];
        let mut tail = [0u8; 32];
        let h_len = bytes.len().min(32);
        head[..h_len].copy_from_slice(&bytes[..h_len]);
        let t_start = bytes.len().saturating_sub(32);
        let t_len = bytes.len() - t_start;
        tail[..t_len].copy_from_slice(&bytes[t_start..]);
        Self {
            hash: hasher.finish(),
            len: bytes.len(),
            head,
            tail,
        }
    }
}

#[derive(Default)]
struct OversizedSegmentCache {
    map: HashMap<OversizedSegmentKey, Arc<[ChatBodySegment]>>,
    order: VecDeque<OversizedSegmentKey>,
}

#[derive(Default)]
struct SegmentCache {
    map: HashMap<SegmentCacheKey, SegmentCacheEntry>,
    order: VecDeque<SegmentCacheKey>,
}

// A fence may be indented by at most three spaces. Backticks inside code are
// content unless the entire line closes the opening fence.
fn fence(line: &str) -> Option<(char, usize, &str)> {
    let trimmed = line.trim_start_matches(' ');
    if line.len() - trimmed.len() > 3 {
        return None;
    }
    let marker = trimmed.chars().next()?;
    if !matches!(marker, '`' | '~') {
        return None;
    }
    let length = trimmed.chars().take_while(|ch| *ch == marker).count();
    if length < 3 {
        return None;
    }
    Some((marker, length, trimmed.get(length..)?.trim()))
}

pub(super) fn parse_chat_segments_cached(text: &str) -> Arc<[ChatBodySegment]> {
    if text.len() > MAX_SEGMENT_CACHE_BYTES {
        let key = OversizedSegmentKey::from_text(text);
        thread_local! {
            static OVERSIZED: RefCell<OversizedSegmentCache> =
                RefCell::new(OversizedSegmentCache::default());
        }
        if let Some(hit) = OVERSIZED.with(|cell| cell.borrow().map.get(&key).map(Arc::clone)) {
            return hit;
        }
        let segs: Arc<[ChatBodySegment]> = Arc::from(parse_chat_segments_uncached(text));
        OVERSIZED.with(|cell| {
            let mut cache = cell.borrow_mut();
            if !cache.map.contains_key(&key) {
                while cache.map.len() >= MAX_OVERSIZED_CACHE_ENTRIES {
                    let Some(oldest) = cache.order.pop_front() else {
                        break;
                    };
                    cache.map.remove(&oldest);
                }
                cache.order.push_back(key.clone());
            }
            cache.map.insert(key, Arc::clone(&segs));
        });
        return segs;
    }
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut hasher);
    let key = (hasher.finish(), text.len());

    thread_local! {
        static CACHE: RefCell<SegmentCache> = RefCell::new(SegmentCache::default());
    }

    if let Some(hit) = CACHE.with(|cell| {
        cell.borrow()
            .map
            .get(&key)
            .filter(|(raw, _)| raw == text)
            .map(|(_, segs)| Arc::clone(segs))
    }) {
        return hit;
    }

    let segs: Arc<[ChatBodySegment]> = Arc::from(parse_chat_segments_uncached(text));
    CACHE.with(|cell| {
        let mut cache = cell.borrow_mut();
        if !cache.map.contains_key(&key) {
            while cache.map.len() >= MAX_SEGMENT_CACHE_ENTRIES {
                let Some(oldest) = cache.order.pop_front() else {
                    break;
                };
                cache.map.remove(&oldest);
            }
            cache.order.push_back(key);
        }
        cache.map.insert(key, (text.to_owned(), Arc::clone(&segs)));
    });
    segs
}

#[cfg(test)]
pub(super) fn parse_chat_segments(text: &str) -> Vec<ChatBodySegment> {
    parse_chat_segments_cached(text).to_vec()
}

fn parse_chat_segments_uncached(text: &str) -> Vec<ChatBodySegment> {
    if !text.contains("```") && !text.contains("~~~") {
        let trimmed = text.trim_matches('\n');
        let out = if trimmed.is_empty() { text } else { trimmed };
        return vec![ChatBodySegment::Text(SharedString::from(out.to_owned()))];
    }
    let mut segments = Vec::new();
    let mut lines = Vec::new();
    let mut opening: Option<(char, usize, String)> = None;
    for line in text.split('\n') {
        if let Some((marker, length, suffix)) = fence(line) {
            if let Some((open_marker, open_length, _)) = &opening {
                if marker == *open_marker && length >= *open_length && suffix.is_empty() {
                    if let Some((_, _, lang)) = opening.take() {
                        segments.push(ChatBodySegment::Code {
                            lang: SharedString::from(lang),
                            code: SharedString::from(lines.join("\n")),
                        });
                        lines.clear();
                    }
                    continue;
                }
            } else if marker != '`' || !suffix.contains('`') {
                let prose = lines.join("\n");
                let trimmed = prose.trim_matches('\n');
                if !trimmed.is_empty() {
                    segments.push(ChatBodySegment::Text(SharedString::from(
                        trimmed.to_owned(),
                    )));
                }
                lines.clear();
                opening = Some((marker, length, suffix.to_owned()));
                continue;
            }
        }
        lines.push(line);
    }
    if let Some((_, _, lang)) = opening {
        segments.push(ChatBodySegment::Code {
            lang: SharedString::from(lang),
            code: SharedString::from(lines.join("\n")),
        });
    } else {
        let prose = lines.join("\n");
        let trimmed = prose.trim_matches('\n');
        if !trimmed.is_empty() {
            segments.push(ChatBodySegment::Text(SharedString::from(
                trimmed.to_owned(),
            )));
        }
    }
    if segments.is_empty() {
        segments.push(ChatBodySegment::Text(SharedString::from(text.to_owned())));
    }
    segments
}

/// Normalizes a fenced code block language tag into a concise display label.
/// Returns `None` when the fence carries no meaningful language identifier.
pub(super) fn normalize_code_language(tag: &str) -> Option<String> {
    let token = tag
        .split(|c: char| c.is_whitespace() || c == ',' || c == ';')
        .next()
        .unwrap_or("")
        .trim_matches(|c: char| matches!(c, '{' | '}' | '`' | '"' | '\''))
        .trim();
    if token.is_empty() {
        return None;
    }
    let normalized = match token.to_ascii_lowercase().as_str() {
        "ts" | "typescript" => "TypeScript",
        "tsx" => "TSX",
        "js" | "javascript" | "mjs" | "cjs" => "JavaScript",
        "jsx" => "JSX",
        "py" | "python" | "python3" => "Python",
        "rs" | "rust" => "Rust",
        "sh" | "shell" => "Shell",
        "bash" => "Bash",
        "zsh" => "Zsh",
        "fish" => "Fish",
        "json" | "jsonc" | "json5" => "JSON",
        "yaml" | "yml" => "YAML",
        "toml" => "TOML",
        "md" | "markdown" | "mdx" => "Markdown",
        "sql" => "SQL",
        "css" => "CSS",
        "scss" => "SCSS",
        "html" | "htm" => "HTML",
        "xml" | "svg" => "XML",
        "go" | "golang" => "Go",
        "c" => "C",
        "cpp" | "c++" | "cxx" | "cc" => "C++",
        "java" => "Java",
        "kt" | "kotlin" => "Kotlin",
        "swift" => "Swift",
        "rb" | "ruby" => "Ruby",
        "diff" | "patch" => "Diff",
        "dockerfile" | "docker" => "Dockerfile",
        "text" | "txt" | "plaintext" | "plain" => {
            return Some(
                krill_app_core::i18n::catalog::chat::PLAIN_TEXT
                    .get()
                    .to_owned(),
            );
        }
        _ => return Some(token.to_owned()),
    };
    Some(normalized.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_code_language_formats_known_tags_and_drops_empty() {
        assert_eq!(normalize_code_language("").as_deref(), None);
        assert_eq!(normalize_code_language("   \t ").as_deref(), None);
        assert_eq!(normalize_code_language("{}").as_deref(), None);
        assert_eq!(normalize_code_language("rs").as_deref(), Some("Rust"));
        assert_eq!(normalize_code_language("rust").as_deref(), Some("Rust"));
        assert_eq!(
            normalize_code_language("ts title=\"app.ts\"").as_deref(),
            Some("TypeScript")
        );
        assert_eq!(normalize_code_language("tsx").as_deref(), Some("TSX"));
        assert_eq!(normalize_code_language("sh").as_deref(), Some("Shell"));
        assert_eq!(normalize_code_language("bash").as_deref(), Some("Bash"));
        assert_eq!(normalize_code_language("zig").as_deref(), Some("zig"));
    }

    #[test]
    fn fences_preserve_blank_code_lines_and_nested_backticks() {
        assert_eq!(
            parse_chat_segments("before\n````md\n\n```rust\nlet 한글 = 1;\n```\n\n````\nafter"),
            vec![
                ChatBodySegment::Text("before".into()),
                ChatBodySegment::Code {
                    lang: "md".into(),
                    code: "\n```rust\nlet 한글 = 1;\n```\n".into()
                },
                ChatBodySegment::Text("after".into()),
            ]
        );
    }

    #[test]
    fn streaming_fences_and_literal_inline_fences_do_not_drop_text() {
        assert_eq!(
            parse_chat_segments("~~~sh\n\nprintf 'hi'"),
            vec![ChatBodySegment::Code {
                lang: "sh".into(),
                code: "\nprintf 'hi'".into()
            }]
        );
        assert_eq!(
            parse_chat_segments("use ```inline``` here"),
            vec![ChatBodySegment::Text("use ```inline``` here".into())]
        );
    }
    proptest::proptest! {
        #[test]
        fn arbitrary_unicode_and_unfinished_fences_never_panic(text in ".{0,4096}") {
            let segments = parse_chat_segments(&text);
            proptest::prop_assert!(!segments.is_empty());
        }
    }
}
