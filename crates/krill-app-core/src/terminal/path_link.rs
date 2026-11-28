//! 터미널 출력 속 파일 경로 링크(OWE-663 수용 기준 4, MK-119). 컴파일러·grep·git이 찍는
//! `path`, `path:line`, `path:line:col`을 ⌘호버 밑줄·⌘클릭 대상으로 본다.
//!
//! 동작(존재하는 파일만, URL 제외, 상대 경로는 세션 cwd 기준, git `a/`·`b/` 접두사)은 Orca의 터미널 파일 링크
//! (`.ref/orca/src/renderer/src/components/terminal-pane/terminal-file-link-*.ts`, MIT)를 참고했다. 코드 차용 없음.
//!
//! 비용: 화면 기반 조회는 제한된 이웃 행을 모은다. 공백을 포함한 경로 후보는 최대 64개이며 디스크 확인은 호출자의 백그라운드 작업에서 한다.

use std::path::{Path, PathBuf};

use krill_proto::screen::url::{
    is_cjk_or_hangul, is_fullwidth_punct, scan_url_len, url_scheme_len,
    url_scheme_prefix_len_ascii_ci,
};
use krill_proto::screen::{Cell, Flags, is_bidi_control};

mod input;
pub use input::Input;

/// ⌘ 아래에서 찾은 파일 링크. `column`부터 `cols`칸에 밑줄을 긋는다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathLink {
    pub column: u16,
    pub cols: u16,
    pub path: PathBuf,
    /// 1부터 센 줄·열. 없으면 파일 처음.
    pub line: Option<u32>,
    pub col: Option<u32>,
}

/// ⌘ 아래에서 찾은 평문 웹 URL(`http://`·`https://`). `column`부터 `cols`칸에 밑줄을 긋는다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UrlLink {
    pub column: u16,
    pub cols: u16,
    pub uri: String,
}

/// 경로를 끊는 글자. `:`는 `path:line`에 쓰여 끊지 않는다.
fn breaks(c: char) -> bool {
    c.is_whitespace()
        || c.is_control()
        || is_bidi_control(c)
        || is_fullwidth_punct(c)
        || matches!(
            c,
            '"' | '\'' | '`' | '<' | '>' | '(' | ')' | '[' | ']' | '{' | '}' | '|' | ',' | ';'
        )
}

/// (시작 칸, 글자, 폭). 2칸 글자의 둘째 칸(SPACER)은 앞 글자에 붙인다.
fn row_chars(row: &[Cell]) -> Vec<(u16, char, u16)> {
    if row
        .iter()
        .try_fold(0usize, |count, cell| {
            count
                .checked_add(1 + cell.zerowidth.len())
                .filter(|count| *count <= 16_384)
        })
        .is_none()
    {
        return Vec::new();
    }
    let mut chars: Vec<(u16, char, u16)> = Vec::with_capacity(row.len());
    for (col, cell) in (0u16..).zip(row) {
        if cell.flags.contains(Flags::SPACER) {
            if col > 0
                && row
                    .get(usize::from(col - 1))
                    .is_some_and(|c| c.flags.contains(Flags::WIDE))
                && let Some(last) = chars.iter_mut().rev().find(|t| t.0 == col - 1 && t.2 > 0)
            {
                last.2 = last.2.saturating_add(1);
            }
            continue;
        }
        let ch = if cell.flags.contains(Flags::HIDDEN) {
            ' '
        } else {
            cell.ch
        };
        chars.push((col, ch, 1));
        for &ch in cell
            .zerowidth
            .iter()
            .filter(|_| !cell.flags.contains(Flags::HIDDEN))
        {
            chars.push((col, ch, 0));
        }
    }
    chars
}

/// `row`의 `column` 칸을 덮는 평문 `http://`·`https://`·`cursor://`·`vscode://` URL을 찾는다(OWE-867).
pub fn url_at(row: &[Cell], column: u16) -> Option<UrlLink> {
    let chars = row_chars(row);
    let len = chars.len();
    let mut i = 0;
    while i < len {
        let start = i;
        let rest = chars.get(start..)?;
        let scheme_len = url_scheme_len(rest, |&(_, c, _)| c);
        if scheme_len == 0 {
            i += 1;
            continue;
        }
        let boundary = start == 0
            || chars
                .get(start - 1)
                .is_none_or(|&(_, c, _)| !c.is_ascii_alphanumeric() && c != '_');
        if !boundary {
            i = start + scheme_len;
            continue;
        }
        // 같은 공백 단위 토큰 안에 제어·양방향 위장 문자가 섞여 있으면 전체 후보를 버린다.
        let token_poisoned = rest
            .iter()
            .take_while(|&&(_, c, _)| !c.is_whitespace())
            .any(|&(_, c, _)| c.is_control() || is_bidi_control(c));
        if token_poisoned {
            i = start + scheme_len;
            continue;
        }
        let trimmed_len = scan_url_len(rest, |&(_, c, _)| c);
        if trimmed_len <= scheme_len {
            i = start + scheme_len;
            continue;
        }
        i = start + trimmed_len;
        let Some(token) = rest.get(..trimmed_len) else {
            continue;
        };
        let Some(&(start_col, _, _)) = token.first() else {
            continue;
        };
        let Some(&(last_col, _, last_w)) = token.last() else {
            continue;
        };
        let end_col = token
            .iter()
            .map(|&(col, _, w)| col.saturating_add(w))
            .max()
            .unwrap_or(last_col.saturating_add(last_w));
        if !(start_col..end_col).contains(&column) {
            continue;
        }
        let uri: String = token.iter().map(|t| t.1).collect();
        let valid = gpui::http_client::Url::parse(&uri).is_ok_and(|u| {
            matches!(u.scheme(), "http" | "https" | "cursor" | "vscode")
                && u.host_str()
                    .is_some_and(|h| !h.is_empty() && !h.chars().all(|c| c == '.'))
        });
        if valid {
            return Some(UrlLink {
                column: start_col,
                cols: end_col.saturating_sub(start_col),
                uri,
            });
        }
    }
    None
}

fn trim_str_url_end(mut s: &str, trim_punct: bool) -> &str {
    while let Some(c) = s.chars().next_back() {
        let strip = match c {
            '.' | ',' | ':' | ';' | '!' | '?' | '~' => trim_punct,
            ')' => {
                s.chars().filter(|&ch| ch == ')').count()
                    > s.chars().filter(|&ch| ch == '(').count()
            }
            ']' => {
                s.chars().filter(|&ch| ch == ']').count()
                    > s.chars().filter(|&ch| ch == '[').count()
            }
            _ => false,
        };
        let Some(rest) = strip.then(|| s.strip_suffix(c)).flatten() else {
            break;
        };
        s = rest;
    }
    s
}

/// 외부 브라우저로 넘길 URL을 정규화한다.
///
/// Markdown GFM 자동 링크나 터미널 링크 뒤에 닫힘 괄호·한국어 조사(`)하면`)가 붙은 경우
/// 불균형 꼬리를 잘라내고, 비ASCII 경로·호스트를 percent-encoding/punycode ASCII URL로
/// 변환해 macOS `NSURL::URLWithString:`이 `nil`을 돌려보내는 문제를 막는다.
pub fn sanitize_external_url(raw: &str) -> Option<String> {
    let s = raw.trim();
    if s.is_empty()
        || s.chars()
            .any(|c| c.is_whitespace() || c.is_control() || is_bidi_control(c))
    {
        return None;
    }
    let scheme_len = url_scheme_prefix_len_ascii_ci(s);
    let candidate = if scheme_len > 0 {
        let mut in_path = false;
        let mut seg_delim = false;
        let mut host_ascii_or_delim = false;
        let mut cut = None;
        for (offset, c) in s.char_indices().skip_while(|&(i, _)| i < scheme_len) {
            if matches!(c, '/' | '?' | '#') {
                in_path = true;
                seg_delim = false;
                continue;
            }
            if !in_path {
                if is_cjk_or_hangul(c) && host_ascii_or_delim {
                    cut = Some(offset);
                    break;
                }
                if c.is_ascii_alphanumeric()
                    || matches!(c, ')' | ']' | '}' | '>' | '"' | '\'' | ',' | ';')
                {
                    host_ascii_or_delim = true;
                }
            } else if matches!(c, '=' | '&') {
                seg_delim = false;
            } else if is_cjk_or_hangul(c) && seg_delim {
                cut = Some(offset);
                break;
            } else if matches!(c, ')' | ']' | '}' | '>' | '"' | '\'' | ',' | ';' | ':') {
                seg_delim = true;
            }
        }
        if let Some(offset) = cut {
            s.get(..offset)
                .map_or(s, |prefix| trim_str_url_end(prefix, true))
        } else {
            // CJK 접미사가 없더라도 `(https://example.com)` 끝의 불균형 `)`/`]`는 정리한다.
            trim_str_url_end(s, false)
        }
    } else {
        s
    };
    let parsed = gpui::http_client::Url::parse(candidate).ok()?;
    let allowed = match parsed.scheme() {
        "http" | "https" | "cursor" | "vscode" => parsed
            .host_str()
            .is_some_and(|h| !h.is_empty() && !h.chars().all(|c| c == '.')),
        "mailto" => !parsed.path().is_empty(),
        _ => false,
    };
    if !allowed {
        return None;
    }
    let base = if candidate.is_ascii() {
        candidate.to_owned()
    } else {
        parsed.into()
    };
    Some(percent_encode_nsurl_unsafe(&base))
}

/// Percent-encodes ASCII characters that WHATWG `url::Url` allows in paths/queries/fragments
/// but macOS `NSURL::URLWithString:` (RFC 3986) rejects.
fn percent_encode_nsurl_unsafe(url: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    if !url.bytes().any(is_nsurl_unsafe_ascii) {
        return url.to_owned();
    }
    let mut out = String::with_capacity(url.len() + 8);
    for byte in url.bytes() {
        if is_nsurl_unsafe_ascii(byte) {
            out.push('%');
            out.push(HEX[usize::from(byte >> 4)] as char);
            out.push(HEX[usize::from(byte & 0x0f)] as char);
        } else {
            out.push(byte as char);
        }
    }
    out
}

const fn is_nsurl_unsafe_ascii(b: u8) -> bool {
    matches!(
        b,
        b' ' | b'"' | b'<' | b'>' | b'`' | b'{' | b'}' | b'|' | b'^' | b'\\'
    )
}

/// Sanitizes an untrusted external URL for `cx.open_url`, allowing only `http://` and `https://`
/// schemes with a non-empty authority and percent-encoding non-ASCII characters (so macOS
/// `NSURL::initWithString:` never rejects Korean/CJK URLs or executes arbitrary OS schemes).
#[must_use]
pub fn sanitize_http_url(raw: &str) -> Option<String> {
    let clean = sanitize_external_url(raw)?;
    let rest = if clean
        .get(..8)
        .is_some_and(|p| p.eq_ignore_ascii_case("https://"))
    {
        clean.get(8..)?
    } else if clean
        .get(..7)
        .is_some_and(|p| p.eq_ignore_ascii_case("http://"))
    {
        clean.get(7..)?
    } else {
        return None;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    if authority.is_empty() || authority.starts_with(':') {
        return None;
    }
    Some(clean)
}

/// Safe target resolved from a rendered Markdown/chat link (`http(s)://`, `mailto:`,
/// `cursor://`, `vscode://`, local `file://`, or local filesystem path).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MarkdownLinkTarget {
    /// Sanitized external URL ready for `cx.open_url`.
    ExternalUrl(String),
    /// Resolved local filesystem path with optional 1-based `#L<line>`/`#L<line>C<col>` location.
    LocalPath {
        path: PathBuf,
        line: Option<u32>,
        column: Option<u32>,
        /// True when OS `open` / `ShellExecute` could execute the target rather than viewing it.
        dangerous_executable: bool,
        /// True when the executable extension is plain-text source (safe to open in Krill's editor).
        text_script: bool,
    },
}

/// Returns true when a GPUI link click event should trigger link opening (ignores right-clicks
/// and touch long-presses).
#[must_use]
pub fn should_open_link_click(event: &gpui::ClickEvent) -> bool {
    match event {
        gpui::ClickEvent::Mouse(click) => matches!(
            click.up.button,
            gpui::MouseButton::Left | gpui::MouseButton::Middle
        ),
        gpui::ClickEvent::Keyboard(_) => true,
        gpui::ClickEvent::Touch(click) => !click.long_press,
    }
}

/// Returns true when a local path should be opened by the OS default handler (browser for HTML,
/// Preview/image viewer for images, Finder/Explorer for directories, PDF/media/office viewers)
/// rather than a code editor tab.
#[must_use]
pub fn prefers_system_open(path: &Path) -> bool {
    if path.is_dir() {
        return true;
    }
    let Some(ext) = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
    else {
        return false;
    };
    matches!(
        ext.as_str(),
        "html"
            | "htm"
            | "png"
            | "jpg"
            | "jpeg"
            | "gif"
            | "webp"
            | "bmp"
            | "ico"
            | "tiff"
            | "tif"
            | "avif"
            | "heic"
            | "heif"
            | "svg"
            | "pdf"
            | "mp3"
            | "wav"
            | "m4a"
            | "aac"
            | "ogg"
            | "flac"
            | "opus"
            | "mp4"
            | "m4v"
            | "mov"
            | "webm"
            | "mkv"
            | "avi"
            | "doc"
            | "docx"
            | "xls"
            | "xlsx"
            | "ppt"
            | "pptx"
            | "pages"
            | "numbers"
            | "key"
            | "rtf"
            | "epub"
    )
}

#[cfg(unix)]
fn has_unix_execute_bit(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::metadata(path)
        .is_ok_and(|meta| meta.is_file() && (meta.permissions().mode() & 0o111 != 0))
}

#[cfg(not(unix))]
fn has_unix_execute_bit(_path: &Path) -> bool {
    false
}

fn classify_local_executable(path: &Path) -> (bool, bool) {
    if path.is_dir() {
        // macOS `.app` / `.workflow` / `.action` / `.scptd` bundles are directories on disk
        // that `open` executes.
        let is_exec_bundle = path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|ext| {
                matches!(
                    ext.to_ascii_lowercase().as_str(),
                    "app" | "workflow" | "action" | "scptd" | "mpkg" | "pkg"
                )
            });
        return (is_exec_bundle, false);
    }
    let Some(ext) = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
    else {
        return (has_unix_execute_bit(path), false);
    };
    match ext.as_str() {
        "sh" | "bash" | "zsh" | "fish" | "csh" | "ksh" | "ps1" | "psm1" | "psd1" | "bat"
        | "cmd" | "vbs" | "vbe" | "js" | "jse" | "wsf" | "wsh" | "py" | "rb" | "pl"
        | "applescript" => (true, true),
        "command" | "app" | "pkg" | "dmg" | "mpkg" | "action" | "workflow" | "scpt" | "scptd"
        | "osax" | "terminal" | "inetloc" | "webloc" | "exe" | "com" | "msc" | "msi" | "msp"
        | "scr" | "pif" | "lnk" | "hta" | "cpl" | "reg" | "inf" | "jar" | "jnlp" | "appimage"
        | "desktop" | "run" | "out" | "bin" | "elf" | "macho" | "dylib" | "so" => (true, false),
        _ if !prefers_system_open(path) && has_unix_execute_bit(path) => (true, false),
        _ => (false, false),
    }
}

/// Parses GitHub/editor line anchors such as `L10`, `L10-L20`, `L10C5`, `L10C5-L20C8`, `L10:5`,
/// or bare positive integer `10`. Ignores general HTML anchors like `section-1` or `01c`.
fn parse_line_fragment(fragment: &str) -> (Option<u32>, Option<u32>) {
    let trimmed = fragment.trim();
    if trimmed.is_empty() {
        return (None, None);
    }
    let first = trimmed.split('-').next().unwrap_or(trimmed);
    let body = first
        .strip_prefix('L')
        .or_else(|| first.strip_prefix('l'))
        .unwrap_or(first);
    if body.is_empty() || !body.bytes().next().is_some_and(|b| b.is_ascii_digit()) {
        return (None, None);
    }
    if let Some((line_part, col_part)) = body
        .split_once('C')
        .or_else(|| body.split_once('c'))
        .or_else(|| body.split_once(':'))
    {
        let Ok(line) = line_part.parse::<u32>() else {
            return (None, None);
        };
        let Ok(col) = col_part.parse::<u32>() else {
            return (None, None);
        };
        if line == 0 {
            return (None, None);
        }
        return (Some(line), (col > 0).then_some(col));
    }
    let Ok(line) = body.parse::<u32>() else {
        return (None, None);
    };
    if line == 0 {
        return (None, None);
    }
    (Some(line), None)
}

fn has_non_drive_uri_scheme(s: &str) -> bool {
    let Some((scheme, _)) = s.split_once(':') else {
        return false;
    };
    // Single ASCII letter followed by `:` is a Windows drive path (`C:\...`, `C:/...`), not a URI scheme.
    if scheme.len() <= 1 {
        return false;
    }
    let mut bytes = scheme.bytes();
    bytes.next().is_some_and(|b| b.is_ascii_alphabetic())
        && bytes.all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'-' | b'.'))
}

fn percent_decode_path(raw: &str) -> Option<String> {
    if !raw.contains('%') {
        return None;
    }
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    let mut decoded_any = false;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(hi), Some(lo)) = (hex_val(bytes[i + 1]), hex_val(bytes[i + 2]))
        {
            out.push((hi << 4) | lo);
            i += 3;
            decoded_any = true;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    if !decoded_any {
        return None;
    }
    String::from_utf8(out).ok()
}

const fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

fn is_windows_drive_absolute(s: &str) -> bool {
    matches!(
        s.as_bytes(),
        [drive, b':', b'/' | b'\\', ..] if drive.is_ascii_alphabetic()
    )
}

fn expand_tilde_path(s: &str) -> Option<PathBuf> {
    let rest = s.strip_prefix("~/")?;
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .filter(|h| !h.is_empty())?;
    Some(PathBuf::from(home).join(rest))
}

fn split_trailing_line_col(raw: &str) -> (&str, Option<u32>, Option<u32>) {
    // Preserve Windows drive prefixes like `C:\foo.rs:12:3` by never splitting at index 1.
    let min_colon = if is_windows_drive_absolute(raw) { 2 } else { 0 };
    let Some((before_last, last_part)) = raw.rsplit_once(':') else {
        return (raw, None, None);
    };
    if before_last.len() <= min_colon {
        return (raw, None, None);
    }
    let Ok(last_num) = last_part.parse::<u32>() else {
        return (raw, None, None);
    };
    if last_num == 0 {
        return (raw, None, None);
    }
    if let Some((before_line, line_part)) = before_last.rsplit_once(':')
        && before_line.len() > min_colon
        && let Ok(line_num) = line_part.parse::<u32>()
        && line_num > 0
    {
        return (before_line, Some(line_num), Some(last_num));
    }
    (before_last, Some(last_num), None)
}

fn parse_file_url_path_only(raw_url: &str, allow_posix_fallback: bool) -> Option<PathBuf> {
    // Agents sometimes emit raw spaces inside `file:///Users/.../Application Support/...`.
    let encoded = if raw_url.contains(' ') {
        raw_url.replace(' ', "%20")
    } else {
        raw_url.to_owned()
    };
    let parsed = gpui::http_client::Url::parse(&encoded).ok()?;
    if parsed.scheme() != "file" {
        return None;
    }
    // Reject remote UNC/SMB hosts (`file://evil.example/...`); allow only local files.
    if parsed
        .host_str()
        .is_some_and(|h| !h.is_empty() && !h.eq_ignore_ascii_case("localhost"))
    {
        return None;
    }
    let path = match parsed.to_file_path() {
        Ok(p) => p,
        Err(()) if allow_posix_fallback => {
            // Remote POSIX paths (e.g. `file://localhost/srv/app/main.rs`) cannot be converted
            // via `Url::to_file_path()` on a Windows desktop host, so decode `parsed.path()`.
            let decoded = percent_decode_path(parsed.path())?;
            if !decoded.starts_with('/') {
                return None;
            }
            PathBuf::from(decoded)
        }
        Err(()) => return None,
    };
    if path
        .to_string_lossy()
        .chars()
        .any(|c| c.is_control() || is_bidi_control(c))
    {
        return None;
    }
    Some(path)
}

/// Decodes a `file://` URL (supporting `file://localhost/`, `%20` / UTF-8 percent-encoding,
/// unencoded spaces, `#L10-L20` anchors, and trailing `:line[:col]`).
#[must_use]
pub fn decode_file_url(
    raw: &str,
    check_local_exists: bool,
) -> Option<(PathBuf, Option<u32>, Option<u32>)> {
    let trimmed = raw.trim();
    if trimmed.is_empty()
        || trimmed
            .chars()
            .any(|c| c.is_control() || is_bidi_control(c))
        || !trimmed
            .get(..7)
            .is_some_and(|p| p.eq_ignore_ascii_case("file://"))
    {
        return None;
    }
    let allow_posix_fallback = !check_local_exists;
    let (before_hash, fragment) = match trimmed.split_once('#') {
        Some((head, frag)) => (head, Some(frag)),
        None => (trimmed, None),
    };
    let (mut line, mut column) = fragment.map_or((None, None), parse_line_fragment);
    let mut path = if line.is_none() && fragment.is_some() {
        // When `#` is not a line anchor (e.g. `file:///Users/a/C#notes.md`), preserve literal `#`
        // if that path exists locally or if `before_hash` does not exist.
        let literal_encoded = trimmed
            .split_once('?')
            .map_or(trimmed, |(h, _)| h)
            .replace('#', "%23");
        let literal_path = parse_file_url_path_only(&literal_encoded, allow_posix_fallback);
        let head_without_query = before_hash.split_once('?').map_or(before_hash, |(h, _)| h);
        let head_path = parse_file_url_path_only(head_without_query, allow_posix_fallback);
        match (literal_path, head_path) {
            (Some(lit), Some(head)) if check_local_exists => {
                if lit.exists() || !head.exists() {
                    lit
                } else {
                    head
                }
            }
            (Some(lit), _) if !check_local_exists => lit,
            (_, Some(head)) => head,
            (Some(lit), None) => lit,
            (None, None) => return None,
        }
    } else {
        let without_query = before_hash.split_once('?').map_or(before_hash, |(h, _)| h);
        parse_file_url_path_only(without_query, allow_posix_fallback)?
    };
    if line.is_none()
        && let Some(path_str) = path.to_str()
    {
        let (stripped, l, c) = split_trailing_line_col(path_str);
        if l.is_some() && !stripped.is_empty() && (!check_local_exists || !path.exists()) {
            path = PathBuf::from(stripped);
            line = l;
            column = c;
        }
    }
    Some((path, line, column))
}

/// Resolves a Markdown/chat link URL into either a sanitized external URL or a validated local
/// filesystem path (supporting `file://` with percent-encoded spaces/UTF-8, `#L10-L20` line
/// anchors, unencoded spaces, absolute paths, and `base_dir`-relative paths).
#[must_use]
pub fn resolve_markdown_link(raw: &str, base_dir: Option<&Path>) -> Option<MarkdownLinkTarget> {
    resolve_markdown_link_inner(raw, base_dir, false)
}

/// Resolves a Markdown/chat link URL for a remote session without touching the local filesystem
/// or expanding the local `$HOME` environment variable.
#[must_use]
pub fn resolve_markdown_link_remote(
    raw: &str,
    base_dir: Option<&Path>,
) -> Option<MarkdownLinkTarget> {
    resolve_markdown_link_inner(raw, base_dir, true)
}

fn resolve_markdown_link_inner(
    raw: &str,
    base_dir: Option<&Path>,
    remote: bool,
) -> Option<MarkdownLinkTarget> {
    let trimmed = raw.trim();
    if trimmed.is_empty()
        || trimmed
            .chars()
            .any(|c| c.is_control() || is_bidi_control(c))
    {
        return None;
    }

    if let Some(clean) = sanitize_external_url(trimmed) {
        return Some(MarkdownLinkTarget::ExternalUrl(clean));
    }

    if trimmed
        .get(..7)
        .is_some_and(|p| p.eq_ignore_ascii_case("file://"))
    {
        let (path, line, column) = decode_file_url(trimmed, !remote)?;
        let (dangerous_executable, text_script) = if remote {
            (false, false)
        } else {
            classify_local_executable(&path)
        };
        return Some(MarkdownLinkTarget::LocalPath {
            path,
            line,
            column,
            dangerous_executable,
            text_script,
        });
    }

    if has_non_drive_uri_scheme(trimmed) {
        return None;
    }

    let (before_hash, fragment) = match trimmed.split_once('#') {
        Some((head, frag)) => (head, Some(frag)),
        None => (trimmed, None),
    };
    let (mut line, mut column) = fragment.map_or((None, None), parse_line_fragment);
    let mut path_str = before_hash;
    if line.is_none() {
        let (stripped, l, c) = split_trailing_line_col(path_str);
        if l.is_some() {
            path_str = stripped;
            line = l;
            column = c;
        }
    }
    if path_str.is_empty() {
        return None;
    }

    let candidate_path = if path_str.starts_with("~/") {
        if remote {
            // Never expand the local machine's $HOME for a remote session.
            return None;
        }
        expand_tilde_path(path_str)?
    } else if path_str.starts_with('/') || is_windows_drive_absolute(path_str) {
        let raw_pb = PathBuf::from(path_str);
        if remote {
            if let Some(decoded) = percent_decode_path(path_str)
                && !decoded
                    .chars()
                    .any(|c| c.is_control() || is_bidi_control(c))
            {
                PathBuf::from(&decoded)
            } else {
                raw_pb
            }
        } else if !raw_pb.exists()
            && let Some(decoded) = percent_decode_path(path_str)
            && !decoded
                .chars()
                .any(|c| c.is_control() || is_bidi_control(c))
        {
            let decoded_pb = PathBuf::from(&decoded);
            if decoded_pb.exists() {
                decoded_pb
            } else {
                raw_pb
            }
        } else {
            raw_pb
        }
    } else {
        let base = base_dir?;
        if remote {
            let rel = percent_decode_path(path_str)
                .filter(|d| !d.chars().any(|c| c.is_control() || is_bidi_control(c)))
                .unwrap_or_else(|| path_str.to_owned());
            base.join(rel)
        } else {
            let raw_joined = base.join(path_str);
            if raw_joined.exists() {
                raw_joined
            } else if let Some(decoded) = percent_decode_path(path_str)
                && !decoded
                    .chars()
                    .any(|c| c.is_control() || is_bidi_control(c))
            {
                let decoded_joined = base.join(&decoded);
                if decoded_joined.exists() {
                    decoded_joined
                } else {
                    return None;
                }
            } else {
                return None;
            }
        }
    };

    if candidate_path
        .to_string_lossy()
        .chars()
        .any(|c| c.is_control() || is_bidi_control(c))
    {
        return None;
    }
    let (dangerous_executable, text_script) = if remote {
        (false, false)
    } else {
        classify_local_executable(&candidate_path)
    };
    Some(MarkdownLinkTarget::LocalPath {
        path: candidate_path,
        line,
        column,
        dangerous_executable,
        text_script,
    })
}

/// Opens a Markdown/chat link using `App`-level OS handlers (`cx.open_url`, `cx.open_with_system`,
/// or `cx.reveal_path` for dangerous executables). Used by non-Shell views such as Markdown file
/// previews and Bot Item cards.
pub fn open_markdown_link_with_app(raw: &str, base_dir: Option<&Path>, cx: &mut gpui::App) -> bool {
    let Some(target) = resolve_markdown_link(raw, base_dir) else {
        tracing::warn!(url = raw, "ignoring invalid or disallowed markdown link");
        return false;
    };
    match target {
        MarkdownLinkTarget::ExternalUrl(clean) => {
            cx.open_url(&clean);
            true
        }
        MarkdownLinkTarget::LocalPath {
            path,
            dangerous_executable,
            ..
        } => {
            if !path.exists() {
                tracing::warn!(
                    path = %path.display(),
                    "markdown link target does not exist"
                );
                return false;
            }
            if dangerous_executable {
                cx.reveal_path(&path);
            } else {
                cx.open_with_system(&path);
            }
            true
        }
    }
}

/// 포인터 칸을 덮는 후보 글자열(디스크를 보지 않는다). 글자마다 시작 칸, 마지막 글자 다음 칸을 함께 둔다.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Candidate {
    chars: Vec<char>,
    starts: Vec<u16>,
    end: u16,
}

fn candidate_at(chars: &[(u16, char, u16)], column: u16) -> Option<Candidate> {
    let hit = chars
        .iter()
        .position(|&(c, _, w)| (c..c.saturating_add(w)).contains(&column))?;
    if chars.get(hit).is_none_or(|&(_, ch, _)| breaks(ch)) {
        return None;
    }
    let first = chars
        .get(..hit)?
        .iter()
        .rposition(|&(_, ch, _)| breaks(ch))
        .map_or(0, |i| i + 1);
    let mut last = chars
        .get(hit..)?
        .iter()
        .position(|&(_, ch, _)| breaks(ch))
        .map_or(chars.len(), |i| hit + i);
    // 문장 끝 `.`·`:`(예: "see src/a.rs.", "error in src/a.rs:")는 경로가 아니다.
    while last > first
        && chars
            .get(last - 1)
            .is_some_and(|c| matches!(c.1, '.' | ':'))
    {
        last -= 1;
    }
    candidate(chars.get(first..last)?)
}

fn candidate(token: &[(u16, char, u16)]) -> Option<Candidate> {
    let end = token.iter().map(|&(c, _, w)| c.saturating_add(w)).max()?;
    Some(Candidate {
        chars: token.iter().map(|t| t.1).collect(),
        starts: token.iter().map(|t| t.0).collect(),
        end,
    })
}

/// 따옴표·마크다운 안 경로를 먼저 보고, 공백 경계 조합을 실제 파일 존재 여부로 구분한다.
fn candidates_at(row: &[Cell], column: u16) -> Vec<Candidate> {
    const MAX_CANDIDATES: usize = 64;
    let chars = row_chars(row);
    let Some(hit) = chars
        .iter()
        .position(|&(c, _, w)| (c..c.saturating_add(w)).contains(&column))
    else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (open, close) in [
        ('"', '"'),
        ('\'', '\''),
        ('`', '`'),
        ('[', ']'),
        ('(', ')'),
        ('“', '”'),
        ('‘', '’'),
    ] {
        let Some(start) = chars
            .get(..hit)
            .and_then(|s| s.iter().rposition(|t| t.1 == open))
        else {
            continue;
        };
        let Some(end) = chars
            .get(hit + 1..)
            .and_then(|s| s.iter().position(|t| t.1 == close))
            .map(|i| hit + 1 + i)
        else {
            continue;
        };
        let token = &chars[start + 1..end];
        if !token
            .iter()
            .any(|t| t.1.is_control() || is_bidi_control(t.1))
            && let Some(cand) = candidate(token)
        {
            out.push(cand);
        }
    }
    let hard_break = |c: char| breaks(c) && c != ' ';
    let first = chars[..hit]
        .iter()
        .rposition(|t| hard_break(t.1))
        .map_or(0, |i| i + 1);
    let last = chars[hit..]
        .iter()
        .position(|t| hard_break(t.1))
        .map_or(chars.len(), |i| hit + i);
    let mut starts: Vec<_> = (first..=hit)
        .filter(|&start| {
            !chars[start].1.is_whitespace() && (start == first || chars[start - 1].1 == ' ')
        })
        .collect();
    starts.sort_by_key(|&start| {
        (
            !starts_with_root_or_home(&chars, start),
            std::cmp::Reverse(start),
        )
    });
    starts.truncate(MAX_CANDIDATES);
    let ends: Vec<_> = (hit + 1..=last)
        .rev()
        .filter(|&end| !chars[end - 1].1.is_whitespace() && (end == last || chars[end].1 == ' '))
        .take(MAX_CANDIDATES)
        .collect();
    let mut ranges: Vec<_> = starts
        .into_iter()
        .flat_map(|start| ends.iter().map(move |&end| (start, end)))
        .collect();
    // 절대·홈 경로를 우선하고 같은 시작점에서는 가장 긴 실재 경로를 고른다.
    ranges.sort_by_key(|&(start, end)| {
        (
            !starts_with_root_or_home(&chars, start),
            std::cmp::Reverse(end - start),
        )
    });
    out.extend(
        ranges
            .into_iter()
            .take(MAX_CANDIDATES)
            .filter_map(|(start, end)| candidate(&chars[start..end])),
    );
    out.truncate(MAX_CANDIDATES - 1);
    // 컴파일러 형식은 문장부호를 뺀 기존 후보도 남긴다.
    if let Some(cand) = candidate_at(&chars, column) {
        out.push(cand);
    }
    out.dedup();
    out
}

/// `path[:line[:col]]`를 나눈다. 뒤에 붙은 나머지(grep의 `:내용`)는 버린다. 마지막 값은 링크에 드는 글자 수.
fn split_location(text: &[char]) -> (String, Option<u32>, Option<u32>, usize) {
    let digit_at = |i: usize| text.get(i).is_some_and(char::is_ascii_digit);
    let Some(colon) = (0..text.len()).find(|&i| text.get(i) == Some(&':') && digit_at(i + 1))
    else {
        let used = trim_cjk_suffix_after_ascii_ext(text);
        return (text.iter().take(used).collect(), None, None, used);
    };
    let number = |from: usize| {
        let s: String = text
            .iter()
            .skip(from)
            .take_while(|c| c.is_ascii_digit())
            .collect();
        (s.parse::<u32>().ok(), s.len())
    };
    let (line, n) = number(colon + 1);
    let mut used = colon + 1 + n;
    let mut col = None;
    if text.get(used) == Some(&':') && digit_at(used + 1) {
        let (c, m) = number(used + 1);
        col = c;
        used += 1 + m;
    }
    (text.iter().take(colon).collect(), line, col, used)
}

/// `src/main.rs에서`처럼 ASCII 확장자 뒤에 바로 이어진 한·중·일 조사를 잘라낸다(#30093).
fn trim_cjk_suffix_after_ascii_ext(text: &[char]) -> usize {
    let Some(dot) = text.iter().rposition(|&c| c == '.') else {
        return text.len();
    };
    let after_dot = text.get(dot + 1..).unwrap_or_default();
    if after_dot.first().is_none_or(|c| !c.is_ascii_alphanumeric()) {
        return text.len();
    }
    after_dot
        .iter()
        .position(|&c| is_cjk_or_hangul(c))
        .map_or(text.len(), |off| dot + 1 + off)
}

/// 찾아 볼 실제 경로들(앞이 우선). URL·빈 경로·제어 문자는 링크가 아니다.
pub(super) fn valid_raw_candidate(path: &str) -> bool {
    !path.is_empty()
        && !path.contains("://")
        && !path.starts_with("mailto:")
        && !path.chars().any(|c| c.is_control() || is_bidi_control(c))
}

fn starts_with_root_or_home(chars: &[(u16, char, u16)], start: usize) -> bool {
    if chars
        .get(start)
        .is_some_and(|&(_, ch, _)| matches!(ch, '/' | '~'))
    {
        return true;
    }
    if matches!(
        chars.get(start..start + 2),
        Some(&[(_, '\\', _), (_, '\\', _)])
    ) {
        return true;
    }
    matches!(
        chars.get(start..start + 3),
        Some(&[(_, drive, _), (_, ':', _), (_, '\\' | '/', _)]) if drive.is_ascii_alphabetic()
    )
}

fn is_abs_or_win_root(path: &str) -> bool {
    path.starts_with('/')
        || path.starts_with(r"\\")
        || matches!(
            path.as_bytes(),
            [drive, b':', b'\\' | b'/', ..] if drive.is_ascii_alphabetic()
        )
}

fn resolve(path: &str, cwd: Option<&Path>, home: Option<&Path>) -> Vec<PathBuf> {
    if !valid_raw_candidate(path) {
        return Vec::new();
    }
    if is_abs_or_win_root(path) {
        return vec![PathBuf::from(path)];
    }
    if let Some(rest) = path.strip_prefix("~/").or_else(|| path.strip_prefix(r"~\")) {
        return home.map(|h| h.join(rest)).into_iter().collect();
    }
    let Some(cwd) = cwd else {
        return Vec::new();
    };
    let mut out = vec![cwd.join(path)];
    // `git diff`의 `a/src/x.rs`·`b/src/x.rs`.
    if let Some(rest) = path.strip_prefix("a/").or_else(|| path.strip_prefix("b/")) {
        out.push(cwd.join(rest));
    }
    out
}

/// 디스크 확인 결과 한 칸 캐시: 포인터가 같은 글자 위에서 움직이는 동안 stat을 다시 하지 않는다.
// ponytail: 한 칸 캐시라 파일이 생기거나 지워진 직후 같은 글자열 위에서는 옛 결과를 본다. 포인터가 다른 글자로 가면 다시 본다.
#[derive(Debug, Default)]
pub struct Cache {
    last: Option<Cached>,
    remote_probes: std::collections::HashMap<(Option<PathBuf>, String), Option<PathBuf>>,
}

#[derive(Debug)]
struct Cached {
    candidates: Vec<Candidate>,
    column: u16,
    cwd: Option<PathBuf>,
    home: Option<PathBuf>,
    found: Option<PathLink>,
}

impl Cache {
    /// `row`의 `column` 칸을 덮는 파일 링크. 존재하는 일반 파일만(디렉터리 제외).
    pub fn link_at(&mut self, row: &[Cell], column: u16, cwd: Option<&Path>) -> Option<PathLink> {
        let home = krill_proto::os::home_dir();
        self.link_at_with_home(row, column, cwd, home.as_deref())
    }

    fn link_at_with_home(
        &mut self,
        row: &[Cell],
        column: u16,
        cwd: Option<&Path>,
        home: Option<&Path>,
    ) -> Option<PathLink> {
        self.lookup(candidates_at(row, column), column, cwd, home, &mut 256)
    }

    fn bounded(
        &mut self,
        row: &[Cell],
        column: u16,
        cwd: Option<&Path>,
        cap: usize,
        budget: &mut usize,
        crossing: Option<(u16, u16)>,
    ) -> Option<PathLink> {
        let home = krill_proto::os::home_dir();
        let mut candidates = candidates_at(row, column);
        if let Some((first_end, last_start)) = crossing {
            candidates.retain(|candidate| {
                candidate
                    .starts
                    .first()
                    .is_some_and(|&start| start < first_end)
                    && candidate.end > last_start
            });
        }
        candidates.truncate(cap);
        self.lookup(candidates, column, cwd, home.as_deref(), budget)
    }

    fn lookup(
        &mut self,
        candidates: Vec<Candidate>,
        column: u16,
        cwd: Option<&Path>,
        home: Option<&Path>,
        budget: &mut usize,
    ) -> Option<PathLink> {
        if self.last.as_ref().is_none_or(|last| {
            last.candidates != candidates
                || last.cwd.as_deref() != cwd
                || last.home.as_deref() != home
                || (last.column != column
                    && last.found.as_ref().is_none_or(|link| {
                        !(link.column..link.column.saturating_add(link.cols)).contains(&column)
                    }))
        }) {
            let mut is_file = |path: &PathBuf| {
                // Win32 ignores a trailing dot: do not consume sentence punctuation
                // as part of an existing filename before the trimmed candidate is tried.
                if *budget == 0 || cfg!(windows) && path.to_string_lossy().ends_with('.') {
                    return false;
                }
                *budget -= 1;
                path.is_file()
            };
            let found = candidates.iter().find_map(|cand| {
                let literal: String = cand.chars.iter().collect();
                let exact = resolve(&literal, cwd, home).into_iter().find(&mut is_file);
                let (path, line, col, used) = if let Some(path) = exact {
                    (path, None, None, cand.chars.len())
                } else {
                    let (path, line, col, used) = split_location(&cand.chars);
                    let found = resolve(&path, cwd, home).into_iter().find(&mut is_file)?;
                    (found, line, col, used)
                };
                let end = cand.starts.get(used).copied().map_or(cand.end, |next| {
                    let last = cand
                        .starts
                        .get(used.saturating_sub(1))
                        .copied()
                        .unwrap_or(next);
                    next.max(last.saturating_add(1))
                });
                let start = *cand.starts.first()?;
                if !(start..end).contains(&column) {
                    return None;
                }
                Some(PathLink {
                    column: start,
                    cols: end - start,
                    path,
                    line,
                    col,
                })
            });
            self.last = Some(Cached {
                candidates,
                column,
                cwd: cwd.map(Path::to_owned),
                home: home.map(Path::to_owned),
                found,
            });
        }
        self.last
            .as_ref()?
            .found
            .clone()
            .filter(|link| (link.column..link.column.saturating_add(link.cols)).contains(&column))
    }

    pub(super) fn probe_remote(
        &mut self,
        target: &krill_hosts::HostTarget,
        cwd: Option<&Path>,
        query_paths: &[PathBuf],
    ) -> std::collections::HashMap<String, PathBuf> {
        let cwd_key = cwd.map(Path::to_owned);
        let mut missing = Vec::new();
        for raw in query_paths {
            if let Some(raw_str) = raw.to_str()
                && !self
                    .remote_probes
                    .contains_key(&(cwd_key.clone(), raw_str.to_owned()))
            {
                missing.push(raw.clone());
            }
        }
        if !missing.is_empty() {
            let probed = crate::remote_fs::probe_paths(target, cwd, &missing);
            for (raw, found) in missing.into_iter().zip(probed) {
                if let Some(raw_str) = raw.to_str() {
                    self.remote_probes
                        .insert((cwd_key.clone(), raw_str.to_owned()), found);
                }
            }
        }
        let mut resolved = std::collections::HashMap::new();
        for raw in query_paths {
            if let Some(raw_str) = raw.to_str()
                && let Some(Some(found_path)) = self
                    .remote_probes
                    .get(&(cwd_key.clone(), raw_str.to_owned()))
            {
                resolved.insert(raw_str.to_owned(), found_path.clone());
            }
        }
        resolved
    }
}

fn bounded_candidates(
    row: &[Cell],
    column: u16,
    cap: usize,
    crossing: Option<(u16, u16)>,
) -> Vec<Candidate> {
    let mut candidates = candidates_at(row, column);
    if let Some((first_end, last_start)) = crossing {
        candidates.retain(|candidate| {
            candidate
                .starts
                .first()
                .is_some_and(|&start| start < first_end)
                && candidate.end > last_start
        });
    }
    candidates.truncate(cap);
    candidates
}

fn collect_remote_queries(
    candidates: &[Candidate],
    out: &mut Vec<PathBuf>,
    seen: &mut std::collections::HashSet<String>,
    max_queries: usize,
) {
    for cand in candidates {
        if out.len() >= max_queries {
            break;
        }
        let literal: String = cand.chars.iter().collect();
        if valid_raw_candidate(&literal) && seen.insert(literal.clone()) {
            out.push(PathBuf::from(&literal));
            if out.len() >= max_queries {
                break;
            }
        }
        let (split, _, _, _) = split_location(&cand.chars);
        if split != literal && valid_raw_candidate(&split) && seen.insert(split.clone()) {
            out.push(PathBuf::from(split));
        }
    }
}

fn lookup_remote(
    candidates: &[Candidate],
    column: u16,
    resolved: &std::collections::HashMap<String, PathBuf>,
) -> Option<PathLink> {
    candidates.iter().find_map(|cand| {
        let literal: String = cand.chars.iter().collect();
        let (path, line, col, used) = if let Some(path) = resolved.get(&literal) {
            (path.clone(), None, None, cand.chars.len())
        } else {
            let (raw, line, col, used) = split_location(&cand.chars);
            let found = resolved.get(&raw)?.clone();
            (found, line, col, used)
        };
        let end = cand.starts.get(used).copied().map_or(cand.end, |next| {
            let last = cand
                .starts
                .get(used.saturating_sub(1))
                .copied()
                .unwrap_or(next);
            next.max(last.saturating_add(1))
        });
        let start = *cand.starts.first()?;
        if !(start..end).contains(&column) {
            return None;
        }
        Some(PathLink {
            column: start,
            cols: end - start,
            path,
            line,
            col,
        })
    })
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod windows_path_tests {
    use super::*;

    #[test]
    fn resolves_windows_drive_unc_and_tilde_backslash_paths() {
        assert_eq!(
            resolve(r"C:\src\main.rs", None, None),
            vec![PathBuf::from(r"C:\src\main.rs")]
        );
        assert_eq!(
            resolve("d:/repo/src/lib.rs", None, None),
            vec![PathBuf::from("d:/repo/src/lib.rs")]
        );
        assert_eq!(
            resolve(r"\\server\share\main.rs", None, None),
            vec![PathBuf::from(r"\\server\share\main.rs")]
        );
        assert_eq!(
            resolve(r"~\.claude\settings.json", None, Some(Path::new("/home/u"))),
            vec![Path::new("/home/u").join(r".claude\settings.json")]
        );

        let cells: Vec<Cell> = r"error at C:\Program Files\App\main.rs:12"
            .chars()
            .map(|ch| Cell {
                ch,
                ..Cell::default()
            })
            .collect();
        let cands = candidates_at(&cells, 25);
        assert!(
            cands.first().is_some_and(|c| {
                let s: String = c.chars.iter().collect();
                s.starts_with(r"C:\Program Files\App\main.rs")
            }),
            "Windows drive path should be prioritized first: {cands:?}"
        );
    }

    #[test]
    fn chat_18_32_unix_exec_bit_and_remote_link_and_file_url_line_col() {
        // file://localhost + :line:col decoding
        let (path, line, col) =
            decode_file_url("file://localhost/srv/app/src/main.rs:42:7", false).unwrap();
        assert_eq!(path, PathBuf::from("/srv/app/src/main.rs"));
        assert_eq!(line, Some(42));
        assert_eq!(col, Some(7));

        // Remote resolution skips local filesystem existence checks and local $HOME expansion.
        let remote =
            resolve_markdown_link_remote("src/lib.rs:10:3", Some(Path::new("/remote/repo")));
        assert_eq!(
            remote,
            Some(MarkdownLinkTarget::LocalPath {
                path: Path::new("/remote/repo").join("src/lib.rs"),
                line: Some(10),
                column: Some(3),
                dangerous_executable: false,
                text_script: false,
            })
        );
        // Remote tilde path returns None (never leaks local $HOME).
        let remote_tilde =
            resolve_markdown_link_remote("~/notes.md", Some(Path::new("/remote/repo")));
        assert_eq!(remote_tilde, None);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let dir = std::env::temp_dir().join(format!("krill-exec-bit-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            let bin = dir.join("payload_bin");
            std::fs::write(&bin, b"\x7fELF\x02\x01\x01\x00").unwrap();
            let mut perms = std::fs::metadata(&bin).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&bin, perms).unwrap();
            let url = format!("file://{}", bin.display());
            let resolved = resolve_markdown_link(&url, None).unwrap();
            match resolved {
                MarkdownLinkTarget::LocalPath {
                    dangerous_executable,
                    text_script,
                    ..
                } => {
                    assert!(
                        dangerous_executable && !text_script,
                        "Unix +x binary must be classified as dangerous non-script executable"
                    );
                }
                MarkdownLinkTarget::ExternalUrl(_) => panic!("expected LocalPath"),
            }
            std::fs::remove_dir_all(&dir).ok();
        }
    }
}
