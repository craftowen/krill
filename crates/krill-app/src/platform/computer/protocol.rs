//! `krill-computer-worker/1` 와이어 형식: stdin/stdout 한 줄 JSON(NDJSON) 프레임.
//!
//! 프레임 한 줄은 개행을 뺀 [`MAX_FRAME_BYTES`] 이하 UTF-8 JSON이다(양방향). 결과 축(전달·관찰·검증·정리)은
//! 서로 독립이며 한 축의 실패가 다른 축의 사실을 지우지 않는다. 사람이 읽는 문구는 영어
//! 진단 문자열만 담는다(UI 문구 아님).

use std::io::{self, BufRead, Read as _};

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

pub const CONTRACT: &str = "krill-computer-worker/1";
/// 한 프레임(줄) 상한. 넘으면 프로토콜 오류로 워커를 끝낸다(재동기화하지 않는다).
pub const MAX_FRAME_BYTES: usize = 1024 * 1024;
/// 요청 하나의 전체 예산(큐·잠금 대기·실행·검증) 상한. 무제한 요청은 받지 않는다.
pub const MAX_DEADLINE_MS: u64 = 30_000;
/// 새 요청 seq가 지금까지 본 최대값보다 앞설 수 있는 폭.
pub const MAX_SEQ_GAP: u64 = 1024;
/// `set_value`·`type_text` 텍스트 상한(UTF-16 단위).
pub const MAX_TEXT_UTF16: usize = 4096;
/// 관찰 노드 상한(요청이 더 작게 줄일 수 있다).
pub const MAX_OBSERVE_NODES: usize = 2000;
pub const DEFAULT_OBSERVE_NODES: usize = 400;
/// 실행 대기열 길이. 가득 차면 접수하지 않는다(`busy`).
pub const QUEUE_CAPACITY: usize = 4;
/// `batch` 한 요청의 변경 단계 상한.
pub const MAX_BATCH_STEPS: usize = 4;
/// 접수 장부가 기억하는 요청 수. 퇴출된 seq는 `expired`이며 재실행 허가가 아니다.
pub const LEDGER_CAPACITY: usize = 64;
const MAX_ACTION_NAME: usize = 64;
/// 장부 요약에 남기는 오류 코드 상한(바이트).
const MAX_SUMMARY_CODE: usize = 64;
/// 거절·프로토콜 오류의 진단 문구 상한(바이트). 입력을 되풀이하는 파서 오류가 출력 프레임을 키우지 못하게 한다.
const MAX_DIAGNOSTIC_BYTES: usize = 512;

/// 호출자 → 워커.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Inbound {
    Request(RequestFrame),
    /// 요청 상태 조회. 입력을 일으키지 않는다.
    Status {
        seq: u64,
    },
    /// 실행 전 요청만 취소된다. 발행이 시작된 요청은 `too_late`.
    Cancel {
        seq: u64,
    },
    /// 새 접수를 멈추고 대기 중인 요청을 미실행으로 끝낸 뒤 종료한다.
    Shutdown,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestFrame {
    pub epoch: String,
    pub context: String,
    pub seq: u64,
    pub deadline_ms: u64,
    pub op: Op,
}

impl RequestFrame {
    /// 중복 접수 대조용 요청 지문. 같은 seq의 재전송이 같은 요청인지 가른다.
    pub fn digest(&self) -> [u8; 32] {
        // 이 타입의 직렬화는 실패하지 않는다(맵 키가 전부 문자열).
        let bytes = serde_json::to_vec(self).unwrap_or_default();
        Sha256::digest(bytes).into()
    }
}

/// 관찰 참조: `snapshot`은 `epoch/context/generation`, `element`는 그 관찰의 `eN`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Target {
    pub snapshot: String,
    pub element: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Op {
    Capabilities,
    /// 워커 프로세스 자신의 TCC 상태(프롬프트 없음).
    Permissions,
    Apps,
    /// `window`가 없으면 포커스 창. 포커스 창이 없으면 첫 창으로 바꾸지 않고 거절한다.
    Observe {
        pid: i32,
        #[serde(default)]
        window: Option<usize>,
        #[serde(default)]
        limit: Option<usize>,
    },
    Action {
        target: Target,
        name: String,
    },
    SetValue {
        target: Target,
        text: String,
        #[serde(default)]
        verify: bool,
    },
    Click {
        target: Target,
    },
    TypeText {
        target: Target,
        text: String,
    },
    /// 변경 단계 1..=[`MAX_BATCH_STEPS`]개를 한 seq·한 잠금 안에서 차례로 실행한다. 읽기·중첩은 받지 않는다.
    /// 단계마다 재검증하며, 멈추면 뒤 단계는 미실행이고 이미 낸 입력은 되돌리지 않는다.
    Batch {
        steps: Vec<Op>,
    },
}

impl Op {
    /// 데스크톱 입력 잠금이 필요한 변경 동작인지.
    pub const fn is_mutation(&self) -> bool {
        matches!(
            self,
            Self::Action { .. }
                | Self::SetValue { .. }
                | Self::Click { .. }
                | Self::TypeText { .. }
                | Self::Batch { .. }
        )
    }

    /// 접수 시 형식 검사. 실패하면 seq를 예약하지 않고 거절한다.
    pub fn validate(&self) -> Result<(), String> {
        match self {
            Self::Capabilities | Self::Permissions | Self::Apps | Self::Click { .. } => Ok(()),
            Self::Observe { pid, limit, .. } => {
                if *pid <= 0 {
                    return Err("pid must be positive".to_owned());
                }
                if limit.is_some_and(|n| n == 0 || n > MAX_OBSERVE_NODES) {
                    return Err(format!("limit must be 1..={MAX_OBSERVE_NODES}"));
                }
                Ok(())
            }
            Self::Action { name, .. } => {
                if name.is_empty() || name.len() > MAX_ACTION_NAME {
                    return Err("action name must be 1..=64 bytes".to_owned());
                }
                Ok(())
            }
            Self::SetValue { text, .. } | Self::TypeText { text, .. } => {
                if text.encode_utf16().count() > MAX_TEXT_UTF16 {
                    return Err(format!("text exceeds {MAX_TEXT_UTF16} UTF-16 units"));
                }
                if matches!(self, Self::TypeText { .. }) && text.is_empty() {
                    return Err("text must not be empty".to_owned());
                }
                Ok(())
            }
            Self::Batch { steps } => {
                if steps.is_empty() || steps.len() > MAX_BATCH_STEPS {
                    return Err(format!("batch.steps must have 1..={MAX_BATCH_STEPS} steps"));
                }
                for step in steps {
                    if !step.is_mutation() || matches!(step, Self::Batch { .. }) {
                        return Err("batch steps must be single input operations".to_owned());
                    }
                    step.validate()?;
                }
                Ok(())
            }
        }
    }
}

/// 요청 수명 상태. 장부는 앞의 다섯만 저장하고, `expired`·`unseen`은 조회 결과다.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestState {
    Queued,
    Dispatching,
    Completed,
    NotDispatched,
    Unknown,
    /// 장부에서 퇴출됐다. 결과를 모를 뿐 재실행 허가가 아니다.
    Expired,
    /// 이 문맥에서 아직 받은 적 없는 seq.
    Unseen,
}

impl RequestState {
    pub const fn is_live(self) -> bool {
        matches!(self, Self::Queued | Self::Dispatching)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CancelOutcome {
    Cancelled,
    TooLate,
    AlreadyFinished,
    Expired,
    Unseen,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RejectCode {
    StaleEpoch,
    ForeignContext,
    SeqOutOfRange,
    PayloadMismatch,
    Expired,
    InvalidRequest,
    Busy,
    ShuttingDown,
}

/// 입력 전달 축. `issued_units`는 발행한 이벤트·호출 수이며 앱이 받은 양이 아니다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Delivery {
    /// 입력을 발행하지 않는 조회.
    ReadOnly,
    NotDispatched {
        reason: String,
    },
    Submitted {
        #[serde(rename = "issuedUnits")]
        issued_units: u32,
    },
    Partial {
        #[serde(rename = "issuedUnits")]
        issued_units: u32,
        reason: String,
    },
    /// 발행 뒤 결과를 확인할 수 없다. 같은 요청을 다시 보내지 않는다.
    Unknown {
        reason: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Observation {
    NotRequested,
    Observed {
        snapshot: String,
        coverage: Coverage,
    },
    /// 관찰하지 못했다. 시간 초과도 여기서 `reason: "deadline_exceeded"`로 알린다.
    Failed {
        reason: String,
    },
}

/// 관찰 범위. `truncated`면 `reasons`에 이유가 있고 미관찰 부분을 없음(Gone)으로 판정하지 않는다.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Coverage {
    pub nodes: usize,
    pub truncated: bool,
    pub reasons: Vec<String>,
}

impl Coverage {
    pub fn truncate(&mut self, reason: &str) {
        self.truncated = true;
        if !self.reasons.iter().any(|r| r == reason) {
            self.reasons.push(reason.to_owned());
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Verification {
    NotRequested,
    Satisfied { predicate: String },
    Unsatisfied { predicate: String },
    Unknown { reason: String },
}

/// 정리 축. `incomplete`면 남은 입력 상태(눌린 키 등)를 모르므로 결과는 `unknown`이고 실행기는 정지한다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Cleanup {
    NotNeeded,
    Completed,
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "wire cleanup outcome exercised by backend isolation tests"
        )
    )]
    Incomplete {
        reason: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ErrorInfo {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Outcome {
    pub seq: u64,
    pub state: RequestState,
    pub delivery: Delivery,
    pub observation: Observation,
    pub verification: Verification,
    pub cleanup: Cleanup,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

impl Outcome {
    /// 부작용 없이 끝난 요청.
    pub fn not_dispatched(seq: u64, code: &str, message: impl Into<String>) -> Self {
        Self {
            seq,
            state: RequestState::NotDispatched,
            delivery: Delivery::NotDispatched {
                reason: code.to_owned(),
            },
            observation: Observation::NotRequested,
            verification: Verification::NotRequested,
            cleanup: Cleanup::NotNeeded,
            error: Some(ErrorInfo {
                code: code.to_owned(),
                message: message.into(),
            }),
            data: None,
        }
    }

    /// 전달·정리 축으로 최종 상태를 정한다. 발행량을 아는 부분 전달(정리 완료)은 `completed`이며 오류는 축과
    /// `error`로 알린다. 불명 전달·정리 미완료만 `unknown`이다.
    pub const fn state_for(delivery: &Delivery, cleanup: &Cleanup) -> RequestState {
        match (delivery, cleanup) {
            (Delivery::Unknown { .. }, _) | (_, Cleanup::Incomplete { .. }) => {
                RequestState::Unknown
            }
            (Delivery::NotDispatched { .. }, _) => RequestState::NotDispatched,
            (Delivery::ReadOnly | Delivery::Submitted { .. } | Delivery::Partial { .. }, _) => {
                RequestState::Completed
            }
        }
    }
}

/// 장부가 끝난 요청마다 남기는 상한 있는 요약(축 상태 이름·발행 단위·오류 코드). 사용자 데이터·결과는 없다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Summary {
    pub delivery: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub issued_units: Option<u32>,
    pub verification: &'static str,
    pub cleanup: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
}

impl Summary {
    pub fn of(outcome: &Outcome) -> Self {
        let (delivery, issued_units) = match &outcome.delivery {
            Delivery::ReadOnly => ("read_only", None),
            Delivery::NotDispatched { .. } => ("not_dispatched", None),
            Delivery::Submitted { issued_units } => ("submitted", Some(*issued_units)),
            Delivery::Partial { issued_units, .. } => ("partial", Some(*issued_units)),
            Delivery::Unknown { .. } => ("unknown", None),
        };
        Self {
            delivery,
            issued_units,
            verification: match outcome.verification {
                Verification::NotRequested => "not_requested",
                Verification::Satisfied { .. } => "satisfied",
                Verification::Unsatisfied { .. } => "unsatisfied",
                Verification::Unknown { .. } => "unknown",
            },
            cleanup: match outcome.cleanup {
                Cleanup::NotNeeded => "not_needed",
                Cleanup::Completed => "completed",
                Cleanup::Incomplete { .. } => "incomplete",
            },
            error_code: outcome
                .error
                .as_ref()
                .map(|e| bounded(e.code.clone(), MAX_SUMMARY_CODE)),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Limits {
    pub max_frame_bytes: usize,
    pub max_deadline_ms: u64,
    pub max_seq_gap: u64,
    pub max_text_utf16: usize,
    pub max_observe_nodes: usize,
    pub queue_capacity: usize,
    pub ledger_capacity: usize,
    pub max_batch_steps: usize,
}

impl Limits {
    pub const fn current() -> Self {
        Self {
            max_frame_bytes: MAX_FRAME_BYTES,
            max_deadline_ms: MAX_DEADLINE_MS,
            max_seq_gap: MAX_SEQ_GAP,
            max_text_utf16: MAX_TEXT_UTF16,
            max_observe_nodes: MAX_OBSERVE_NODES,
            queue_capacity: QUEUE_CAPACITY,
            ledger_capacity: LEDGER_CAPACITY,
            max_batch_steps: MAX_BATCH_STEPS,
        }
    }
}

/// 워커 → 호출자.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Outbound {
    Hello {
        contract: &'static str,
        epoch: String,
        context: String,
        pid: u32,
        supported: bool,
        capabilities: Vec<&'static str>,
        unsupported: Vec<&'static str>,
        limits: Limits,
    },
    Accepted {
        seq: u64,
    },
    Rejected {
        #[serde(skip_serializing_if = "Option::is_none")]
        seq: Option<u64>,
        code: RejectCode,
        message: String,
    },
    /// 같은 seq·같은 요청의 재전송. 다시 실행하지 않고 현재 상태만 알린다.
    Duplicate {
        seq: u64,
        state: RequestState,
    },
    Result(Box<Outcome>),
    /// `summary`는 장부에 남은 끝난 요청에만 있다.
    Status {
        seq: u64,
        state: RequestState,
        #[serde(skip_serializing_if = "Option::is_none")]
        summary: Option<Summary>,
    },
    Cancel {
        seq: u64,
        outcome: CancelOutcome,
    },
    ProtocolError {
        message: String,
    },
}

/// 한 프레임을 읽는다. EOF면 `Ok(false)`. 상한을 넘으면 `InvalidData` 오류다.
/// 마지막 줄에 개행이 없어도 한 프레임으로 받는다.
pub fn read_frame(reader: &mut impl BufRead, buf: &mut Vec<u8>) -> io::Result<bool> {
    buf.clear();
    // 상한 + 개행 1바이트까지만 읽는다.
    let limit = u64::try_from(MAX_FRAME_BYTES)
        .unwrap_or(u64::MAX)
        .saturating_add(1);
    let read = reader.by_ref().take(limit).read_until(b'\n', buf)?;
    if read == 0 {
        return Ok(false);
    }
    // 개행으로 끝났으면 내용은 상한 이하다. 개행 없이 상한+1바이트면 넘친 것이다.
    if buf.last() == Some(&b'\n') {
        buf.pop();
    } else if buf.len() > MAX_FRAME_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "frame exceeds limit",
        ));
    }
    if buf.last() == Some(&b'\r') {
        buf.pop();
    }
    Ok(true)
}

/// 프레임 한 줄의 해석 결과.
#[derive(Debug, Clone, PartialEq)]
pub enum Parsed {
    /// 빈 줄(공백만). 무시한다.
    Blank,
    Inbound(Inbound),
    /// JSON이지만 계약에 맞지 않는다(미지원 op 등). seq를 예약하지 않고 거절하며 연결은 유지한다.
    Rejected {
        seq: Option<u64>,
        message: String,
    },
}

/// 한 줄을 해석한다. JSON이 아니면 `Err`이며 호출자는 프로토콜 오류로 연결을 끝낸다.
pub fn parse_line(line: &[u8]) -> Result<Parsed, String> {
    if line.iter().all(u8::is_ascii_whitespace) {
        return Ok(Parsed::Blank);
    }
    let value = serde_json::from_slice::<serde_json::Value>(line)
        .map_err(|error| diagnostic(format!("frame is not JSON: {error}")))?;
    Ok(match serde_json::from_value::<Inbound>(value.clone()) {
        Ok(inbound) => Parsed::Inbound(inbound),
        Err(error) => Parsed::Rejected {
            seq: value.get("seq").and_then(serde_json::Value::as_u64),
            message: diagnostic(error.to_string()),
        },
    })
}

/// 진단 문구를 [`MAX_DIAGNOSTIC_BYTES`] 안으로 줄인다(글자 경계에서 자르고 `…`를 붙인다).
pub fn diagnostic(mut message: String) -> String {
    if message.len() > MAX_DIAGNOSTIC_BYTES {
        message = bounded(message, MAX_DIAGNOSTIC_BYTES);
        message.push('…');
    }
    message
}

/// 글자 경계에서 `max`바이트 이하로 자른다.
fn bounded(mut text: String, max: usize) -> String {
    let mut end = text.len().min(max);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text.truncate(end);
    text
}

pub const OUTPUT_TOO_LARGE: &str = "output_too_large";

/// 프레임을 개행으로 끝나는 한 줄로 직렬화한다. 내용은 [`MAX_FRAME_BYTES`] 이하이고 JSON 문자열 이스케이프 덕에
/// 끝 개행 말고 개행이 없다. 상한을 넘는 결과는 `data`를 빼고 관찰을 실패로 바꾼 `output_too_large` 결과로 보낸다
/// (seq·상태·전달·검증·정리 축은 그대로). 그래도 넘거나 결과가 아닌 프레임이 넘으면 `None`이다 — 호출자는
/// 잘라 보내지 않고 전송로 실패로 다룬다.
pub fn encode(frame: &Outbound) -> Option<Vec<u8>> {
    fn line(frame: &Outbound) -> Option<Vec<u8>> {
        let mut bytes = serde_json::to_vec(frame).ok()?;
        if bytes.len() > MAX_FRAME_BYTES {
            return None;
        }
        bytes.push(b'\n');
        Some(bytes)
    }
    if let Some(bytes) = line(frame) {
        return Some(bytes);
    }
    let Outbound::Result(outcome) = frame else {
        return None;
    };
    let mut slim = outcome.clone();
    slim.data = None;
    if matches!(slim.observation, Observation::Observed { .. }) {
        slim.observation = Observation::Failed {
            reason: OUTPUT_TOO_LARGE.to_owned(),
        };
    }
    slim.error = Some(ErrorInfo {
        code: OUTPUT_TOO_LARGE.to_owned(),
        message: format!("result exceeded {MAX_FRAME_BYTES} bytes; data omitted"),
    });
    line(&Outbound::Result(slim))
}

#[cfg(test)]
mod tests;
