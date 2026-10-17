//! CSV/TSV 읽기 전용 미리보기 파서. RFC 4180 따옴표(`""` 이스케이프, 따옴표 안 줄바꿈)와 CR/LF/CRLF를 다룬다.
//! 따옴표는 필드 첫 글자일 때만 인용으로 본다(Orca `csv-parse.ts`와 같은 규칙).

/// 미리보기로 읽는 앞부분 바이트 수. Orca `CSV_PAGED_PREVIEW_BYTES`와 같다.
pub const PREVIEW_BYTES: usize = 1024 * 1024;
/// 미리보기 행 상한(헤더 포함).
/// ponytail: 추정값 — 1MiB 안에 짧은 행이 수십만 개면 `Vec<String>` 오버헤드가 커져서 자른다. 페이지 넘김이 필요하면 바이트 오프셋 색인으로.
pub const MAX_ROWS: usize = 10_000;
/// 미리보기 열 상한. 한 행에 구분자만 수십만 개인 파일이 행마다 그만큼 셀을 그리지 않게 한다(추정값).
pub const MAX_COLUMNS: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Delimiter {
    Comma,
    Tab,
}

impl Delimiter {
    pub fn as_char(self) -> char {
        match self {
            Self::Comma => ',',
            Self::Tab => '\t',
        }
    }
}

/// 첫 번째 내용 있는 줄에서 따옴표 밖 탭이 쉼표보다 많으면 탭(동점은 쉼표). BOM은 무시.
pub fn sniff(text: &str) -> Delimiter {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let line = text
        .split(['\r', '\n'])
        .find(|l| !l.trim().is_empty())
        .unwrap_or("");
    let (mut tabs, mut commas, mut quoted) = (0usize, 0usize, false);
    for c in line.chars() {
        match c {
            '"' => quoted = !quoted,
            '\t' if !quoted => tabs += 1,
            ',' if !quoted => commas += 1,
            _ => {}
        }
    }
    if tabs > commas {
        Delimiter::Tab
    } else {
        Delimiter::Comma
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Table {
    pub rows: Vec<Vec<String>>,
    /// 가장 긴 행의 열 수.
    pub columns: usize,
    /// 파일 앞부분만 읽었거나 행·열 상한에 걸렸다.
    pub truncated: bool,
}

/// `head`는 파일 앞 최대 [`PREVIEW_BYTES`], `file_len`은 전체 길이. 앞부분만 받았으면 잘렸을 수 있는 마지막 레코드를 버린다.
pub fn preview(head: &[u8], file_len: u64, delimiter: Delimiter) -> Table {
    let complete = u64::try_from(head.len()).is_ok_and(|n| n >= file_len);
    let mut table = parse(
        &String::from_utf8_lossy(head),
        delimiter.as_char(),
        complete,
    );
    table.truncated |= !complete;
    table
}

fn parse(text: &str, delimiter: char, complete: bool) -> Table {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut table = Table::default();
    let mut row: Vec<String> = Vec::new();
    let mut field = String::new();
    // `started`: 이 필드에 글자나 여는 따옴표가 나왔다(이후 따옴표는 글자). `content`: 이 레코드가 빈 줄이 아니다.
    let (mut quoted, mut started, mut content) = (false, false, false);
    let mut chars = text.chars().peekable();
    let push_field = |row: &mut Vec<String>, field: &mut String, truncated: &mut bool| {
        if row.len() < MAX_COLUMNS {
            row.push(std::mem::take(field));
        } else {
            field.clear();
            *truncated = true;
        }
    };
    while let Some(c) = chars.next() {
        if table.rows.len() >= MAX_ROWS {
            table.truncated = true;
            return finish(table);
        }
        if quoted {
            if c == '"' {
                if chars.next_if_eq(&'"').is_some() {
                    field.push('"');
                } else {
                    quoted = false;
                }
            } else {
                field.push(c);
            }
            continue;
        }
        match c {
            '"' if !started => {
                (quoted, started, content) = (true, true, true);
            }
            '\r' | '\n' => {
                if c == '\r' {
                    chars.next_if_eq(&'\n');
                }
                push_field(&mut row, &mut field, &mut table.truncated);
                table.rows.push(std::mem::take(&mut row));
                (started, content) = (false, false);
            }
            c if c == delimiter => {
                push_field(&mut row, &mut field, &mut table.truncated);
                (started, content) = (false, true);
            }
            c => {
                field.push(c);
                (started, content) = (true, true);
            }
        }
    }
    // 끝에 줄바꿈이 없는 마지막 레코드. 앞부분만 읽었으면 중간에서 잘렸을 수 있어 버린다.
    if complete && (content || !row.is_empty()) {
        push_field(&mut row, &mut field, &mut table.truncated);
        table.rows.push(row);
    }
    finish(table)
}

fn finish(mut table: Table) -> Table {
    table.columns = table.rows.iter().map(Vec::len).max().unwrap_or(0);
    table
}
