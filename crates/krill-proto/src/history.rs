//! MK-075 기록 삭제. Hello에서 `FEAT_HISTORY`를 협상한 뒤 전용 연결에서 요청 하나·응답 하나.
//! 모든 세션의 디스크 세그먼트만 지우므로 외부(Full scope) 클라이언트만 허용한다.

use crate::screen::Msg;
use serde::{Deserialize, Serialize};

/// 로컬 기록 초기화 실패 표시. 내용에는 경로·키·터미널 본문을 넣지 않는다.
pub const RECORDING_WARNING_FILE: &str = "history-warning";

pub const FEAT_HISTORY: u64 = 1 << 16;
/// 기록 일시중지 명령. 구 데몬은 광고하지 않으므로 앱은 요청 전에 지원 여부를 확인한다.
pub const FEAT_HISTORY_PAUSE: u64 = 1 << 55;

pub mod kind {
    pub const HISTORY_REQ: u8 = 0x40;
    pub const HISTORY_REPLY: u8 = 0x60;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HistoryRequest {
    Clear,
    /// 구 데몬은 이 변형을 알 수 없어 연결을 닫는다. 호출자는 Paused ACK 없이는 성공으로 표시하지 않는다.
    SetPaused {
        paused: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HistoryReply {
    Cleared,
    Failed(crate::control::Failure),
    Paused { paused: bool },
}

impl Msg for HistoryRequest {
    const KIND: u8 = kind::HISTORY_REQ;
}
impl Msg for HistoryReply {
    const KIND: u8 = kind::HISTORY_REPLY;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{control::Failure, ipc};

    fn fixture<T: Msg + PartialEq + std::fmt::Debug>(value: &T, expected: &[u8]) {
        assert_eq!(value.encode().unwrap(), expected);
        let (kind, body) = ipc::read_frame(&mut &expected[..]).unwrap().unwrap();
        assert_eq!(&T::decode(kind, &body).unwrap(), value);
    }

    #[test]
    fn fixed_v1_bytes_roundtrip() {
        fixture(&HistoryRequest::Clear, &[2, 0, 0, 0, 0x40, 0]);
        fixture(&HistoryReply::Cleared, &[2, 0, 0, 0, 0x60, 0]);
        fixture(
            &HistoryReply::Failed(Failure::Denied),
            &[3, 0, 0, 0, 0x60, 1, 3],
        );
        fixture(
            &HistoryRequest::SetPaused { paused: true },
            &[3, 0, 0, 0, 0x40, 1, 1],
        );
        fixture(
            &HistoryReply::Paused { paused: false },
            &[3, 0, 0, 0, 0x60, 2, 0],
        );
    }

    proptest::proptest! {
        #[test]
        fn arbitrary_bytes_do_not_panic(bytes in proptest::collection::vec(proptest::prelude::any::<u8>(), 0..128)) {
            if let Ok(request) = HistoryRequest::decode(kind::HISTORY_REQ, &bytes) {
                proptest::prop_assert!(request.encode().is_ok());
            }
            if let Ok(reply) = HistoryReply::decode(kind::HISTORY_REPLY, &bytes) {
                proptest::prop_assert!(reply.encode().is_ok());
            }
        }
    }
}
