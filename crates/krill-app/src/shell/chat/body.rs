//! Fenced code in both completed messages and unfinished streaming output.

#[derive(Debug, PartialEq, Eq)]
pub(super) enum ChatBodySegment {
    Text(String),
    Code { lang: String, code: String },
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

pub(super) fn parse_chat_segments(text: &str) -> Vec<ChatBodySegment> {
    let mut segments = Vec::new();
    let mut lines = Vec::new();
    let mut opening: Option<(char, usize, String)> = None;
    for line in text.split('\n') {
        if let Some((marker, length, suffix)) = fence(line) {
            if let Some((open_marker, open_length, _)) = &opening {
                if marker == *open_marker && length >= *open_length && suffix.is_empty() {
                    if let Some((_, _, lang)) = opening.take() {
                        segments.push(ChatBodySegment::Code {
                            lang,
                            code: lines.join("\n"),
                        });
                        lines.clear();
                    }
                    continue;
                }
            } else if marker != '`' || !suffix.contains('`') {
                let prose = lines.join("\n");
                if !prose.trim_matches('\n').is_empty() {
                    segments.push(ChatBodySegment::Text(prose.trim_matches('\n').to_owned()));
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
            lang,
            code: lines.join("\n"),
        });
    } else {
        let prose = lines.join("\n");
        if !prose.trim_matches('\n').is_empty() {
            segments.push(ChatBodySegment::Text(prose.trim_matches('\n').to_owned()));
        }
    }
    if segments.is_empty() {
        segments.push(ChatBodySegment::Text(text.to_owned()));
    }
    segments
}

#[cfg(test)]
mod tests {
    use super::*;

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
