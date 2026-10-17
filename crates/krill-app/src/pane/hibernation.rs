//! MK-081 자동 재개 경계. 잠정 결정 의존: MK-052 Resume; 거부 시 이 모듈과 `ensure_session_in` 호출을 제거한다.
use krill_proto::SessionId;
use krill_proto::control::{Create, FEAT_HIBERNATION, FEAT_RESUME, Reply, Request};
use std::io;

pub(super) fn wake(
    call: &mut impl FnMut(&Request) -> io::Result<Reply>,
    features: u64,
    session: SessionId,
    create: &Create,
) -> io::Result<Option<SessionId>> {
    if features & (FEAT_HIBERNATION | FEAT_RESUME) != FEAT_HIBERNATION | FEAT_RESUME {
        return Ok(None);
    }
    match call(&Request::Hibernation {
        session,
        create: create.clone(),
    })? {
        Reply::Hibernation { session } => Ok(session),
        Reply::Failed(f) => Err(io::Error::other(f.to_string())),
        _ => Err(io::ErrorKind::InvalidData.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_negotiated_hibernated_sessions_resume() {
        let create = Create {
            account: None,
            program: krill_proto::control::Program::Login,
            cwd: Vec::new(),
            cols: 80,
            rows: 24,
        };
        let mut calls = 0;
        let mut call = |request: &Request| {
            calls += 1;
            assert_eq!(
                request,
                &Request::Hibernation {
                    session: SessionId(7),
                    create: create.clone()
                }
            );
            Ok(Reply::Hibernation {
                session: Some(SessionId(9)),
            })
        };
        assert_eq!(
            wake(&mut call, FEAT_RESUME, SessionId(7), &create).unwrap(),
            None
        );
        assert_eq!(
            wake(
                &mut call,
                FEAT_HIBERNATION | FEAT_RESUME,
                SessionId(7),
                &create
            )
            .unwrap(),
            Some(SessionId(9))
        );
        assert_eq!(calls, 1);
        assert_eq!(
            wake(
                &mut |_| Ok(Reply::Hibernation { session: None }),
                FEAT_HIBERNATION | FEAT_RESUME,
                SessionId(7),
                &create
            )
            .unwrap(),
            None
        );
    }
}
