//! Resource Manager 경량판(MK-071). [`crate::control`]의 REQUEST/REPLY 위에서 쓰는 페이로드다.
//!
//! - [`crate::control::Request::Resources`]`(`[`Action::Query`]`)`: krilld가 재는 메모리·디스크를
//!   [`crate::control::Reply::Resources`]로 답한다. 메모리는 모두 커널 `phys_footprint`(`proc_pid_rusage`,
//!   `footprint(1)`과 같은 값)다. 세션 목록은 요청자의 세션 범위(MK-036)를 따른다.
//! - [`Action::RestartDaemon`]: 전체 범위의 interactive 클라이언트만. krilld가 holder에 자기 교체(`UpgradeKrilld`)를
//!   맡기고 답을 보낸 뒤 끝난다. holder가 같은 실행 파일로 바로 다시 띄우고 세션을 넘긴다(MK-068과 같은 경로).
//!   확인 대화상자는 클라이언트 몫이다.
//!
//! 이전 krilld는 variant를 역직렬화하지 못하므로 클라이언트는 [`FEAT_RESOURCES`]를 협상했을 때만 보낸다.

use serde::{Deserialize, Serialize};

use crate::SessionId;
use crate::holder::MAX_SESSIONS;

/// Resource Manager 요청·응답. [`crate::control::FEAT_CONTROL`]과 함께 협상해야 한다(OWE-436 지휘자 배정).
pub const FEAT_RESOURCES: u64 = 1 << 38;

/// 무엇을 할지. variant는 끝에만 붙인다.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Action {
    Query,
    RestartDaemon,
}

/// 한 시점의 측정값. 바이트 단위. 필드는 끝에만 붙인다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Resources {
    /// 요청한 프로세스(앱·CLI, `LOCAL_PEERPID`). 못 읽으면 `None`.
    pub client: Option<u64>,
    pub daemon: u64,
    /// krilld의 부모 krill-holder. holder 없이 떴거나 못 읽으면 `None`.
    pub holder: Option<u64>,
    /// 상태 디렉터리(세그먼트·로그·크래시) 디스크 사용량(할당 블록).
    pub disk: u64,
    #[serde(deserialize_with = "sessions")]
    pub sessions: Vec<SessionUsage>,
}

/// 세션 루트와 그 후손 프로세스(에이전트 포함)의 footprint 합. setsid로 계보를 떠난 프로세스는 빠진다.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionUsage {
    pub session: SessionId,
    pub footprint: u64,
    /// 합에 든 프로세스 수. 끝난 세션은 0.
    pub processes: u32,
}

impl Resources {
    /// 앱·데몬(krilld+holder)·세션 합. 세션은 앱·데몬과 겹치지 않는다.
    #[must_use]
    pub fn total(&self) -> u64 {
        let sessions = self.sessions.iter().map(|s| s.footprint);
        [
            self.client.unwrap_or(0),
            self.daemon,
            self.holder.unwrap_or(0),
        ]
        .into_iter()
        .chain(sessions)
        .fold(0, u64::saturating_add)
    }
}

fn sessions<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<SessionUsage>, D::Error> {
    crate::control::bounded::<D, SessionUsage, MAX_SESSIONS>(d)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::{Reply, Request};
    use crate::screen::Msg;

    fn sample() -> Resources {
        Resources {
            client: Some(100),
            daemon: 2000,
            holder: None,
            disk: 1 << 20,
            sessions: vec![SessionUsage {
                session: SessionId(3),
                footprint: 300,
                processes: 2,
            }],
        }
    }

    /// 고정 바이트(control variant 번호와 무관한 페이로드만): 바뀌면 호환 위반이다.
    #[test]
    fn fixture_and_roundtrip() {
        let bytes = postcard::to_allocvec(&sample()).unwrap();
        let hex = bytes.iter().fold(String::new(), |mut s, b| {
            use std::fmt::Write as _;
            write!(s, "{b:02x}").unwrap();
            s
        });
        assert_eq!(hex, "0164d00f008080400103ac0202");
        assert_eq!(postcard::from_bytes::<Resources>(&bytes).unwrap(), sample());
        for a in [Action::Query, Action::RestartDaemon] {
            let m = Request::Resources(a);
            let enc = m.encode().unwrap();
            let (k, body) = crate::ipc::read_frame(&mut &enc[..]).unwrap().unwrap();
            assert_eq!(Request::decode(k, &body).unwrap(), m);
        }
        let m = Reply::Resources(sample());
        let enc = m.encode().unwrap();
        let (k, body) = crate::ipc::read_frame(&mut &enc[..]).unwrap().unwrap();
        assert_eq!(Reply::decode(k, &body).unwrap(), m);
    }

    #[test]
    fn total_and_session_bound() {
        assert_eq!(sample().total(), 2400);
        let mut many = sample();
        many.sessions = vec![many.sessions[0]; MAX_SESSIONS + 1];
        let bytes = postcard::to_allocvec(&many).unwrap();
        assert!(postcard::from_bytes::<Resources>(&bytes).is_err());
    }
}
