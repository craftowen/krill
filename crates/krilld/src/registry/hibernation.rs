//! MK-081 재개 경로: 예전 opt-in 자동 종료가 남긴 `hibernated/<세션 ID>` 표식이 있는 pane을
//! 열면 같은 대화로 한 번만 재개한다. 자동 종료 타이머는 0.3에서 제거했다(세션은 Krill이 끝내지 않는다).
use std::fs;
use std::path::PathBuf;

use krill_proto::SessionId;

use super::{Registry, lock};

#[derive(Default)]
pub(super) struct Clock {
    woken: std::collections::HashMap<SessionId, SessionId>,
}

impl Registry {
    fn sleep_path(&self, id: SessionId) -> Option<PathBuf> {
        Some(self.dir.as_ref()?.join("hibernated").join(id.0.to_string()))
    }

    pub(super) fn sleeping(&self, id: SessionId) -> bool {
        self.sleep_path(id).is_some_and(|p| p.is_file())
    }

    pub(super) fn clear_sleeping(&self, id: SessionId) {
        if let Some(path) = self.sleep_path(id) {
            fs::remove_file(path).ok();
        }
    }

    pub(super) fn wake_hibernated(
        &self,
        peer: &crate::scope::Peer,
        old: SessionId,
        create: krill_proto::control::Create,
    ) -> Result<krill_proto::control::Reply, krill_proto::control::Failure> {
        use krill_proto::control::{Failure, Reply};
        // 동시에 두 창이 열려도 한 번만 재개한다.
        let mut clock = lock(&self.hibernation);
        if !peer.authorize("hibernation", old, &self.audit) {
            return Err(Failure::Denied);
        }
        if let Some(&session) = clock.woken.get(&old) {
            return Ok(Reply::Hibernation {
                session: Some(session),
            });
        }
        if !self.sleeping(old) {
            return Ok(Reply::Hibernation { session: None });
        }
        match self.resume(peer, old, create)? {
            Reply::Resumed { session, .. } => {
                clock.woken.insert(old, session);
                Ok(Reply::Hibernation {
                    session: Some(session),
                })
            }
            _ => Err(Failure::BadRequest),
        }
    }
}

#[cfg(test)]
#[cfg(unix)]
mod tests;
