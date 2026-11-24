//! Jupyter 노트북(nbformat 4 JSON) → 읽기 전용 셀 목록. 출력은 `stream`·`text/plain`·오류 요약만 텍스트로 두고
//! 나머지(이미지·HTML 등)는 MIME 이름만 남겨 "생략" 표시한다. 필드 해석은 Orca `ipynb-parse.ts`를 참고했다.

use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notebook {
    pub cells: Vec<Cell>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cell {
    pub kind: CellKind,
    pub source: String,
    pub execution_count: Option<u64>,
    pub outputs: Vec<Output>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CellKind {
    Markdown,
    Code,
    Raw,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Output {
    /// `stream` 출력(stdout/stderr).
    Stream { name: String, text: String },
    /// `execute_result`·`display_data`의 `text/plain`.
    Text(String),
    /// `error` 출력의 `ename: evalue`. traceback은 ANSI 색 코드라 싣지 않는다.
    Error { name: String, message: String },
    /// 텍스트로 보여줄 수 없는 출력(이미지 등). 가장 앞 MIME 이름.
    Omitted(String),
}

#[derive(Debug)]
pub enum ParseError {
    Json(serde_json::Error),
    /// `nbformat`이 4가 아니거나 `cells`가 배열이 아니다.
    Unsupported,
}

pub fn parse(text: &str) -> Result<Notebook, ParseError> {
    let root: Value = serde_json::from_str(text).map_err(ParseError::Json)?;
    if root.get("nbformat").and_then(Value::as_u64) != Some(4) {
        return Err(ParseError::Unsupported);
    }
    let cells = root
        .get("cells")
        .and_then(Value::as_array)
        .ok_or(ParseError::Unsupported)?;
    Ok(Notebook {
        cells: cells.iter().filter_map(cell).collect(),
    })
}

fn cell(raw: &Value) -> Option<Cell> {
    let kind = match raw.get("cell_type")?.as_str()? {
        "markdown" => CellKind::Markdown,
        "code" => CellKind::Code,
        _ => CellKind::Raw,
    };
    let outputs = raw
        .get("outputs")
        .and_then(Value::as_array)
        .map(|outs| outs.iter().filter_map(output).collect())
        .unwrap_or_default();
    Some(Cell {
        kind,
        source: multiline(raw.get("source")),
        execution_count: raw.get("execution_count").and_then(Value::as_u64),
        outputs,
    })
}

fn output(raw: &Value) -> Option<Output> {
    let str_field = |key| {
        raw.get(key)
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned()
    };
    Some(match raw.get("output_type")?.as_str()? {
        "stream" => Output::Stream {
            name: str_field("name"),
            text: multiline(raw.get("text")),
        },
        "error" => Output::Error {
            name: str_field("ename"),
            message: str_field("evalue"),
        },
        _ => {
            let data = raw.get("data")?.as_object()?;
            match data.get("text/plain") {
                Some(text) => Output::Text(multiline(Some(text))),
                None => Output::Omitted(data.keys().next()?.clone()),
            }
        }
    })
}

/// nbformat "multiline string": 문자열 또는 문자열 배열(각 줄이 `\n`을 포함)을 이어 붙인다.
fn multiline(value: Option<&Value>) -> String {
    let text = match value {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(items)) => items.iter().filter_map(Value::as_str).collect(),
        _ => String::new(),
    };
    text.replace("\r\n", "\n")
}
