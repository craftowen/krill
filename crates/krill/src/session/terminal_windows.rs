//! Windows 콘솔 attach는 네이티브 콘솔 어댑터 구현 전까지 명시적으로 거절한다.
use super::{Result, fail};
use crate::remote::Target;
use krill_proto::SessionId;

pub(super) fn size() -> Option<(u16, u16)> {
    None
}

pub(super) fn attach(_target: &Target, _session: SessionId) -> Result<()> {
    Err(fail(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "interactive attach is not supported on Windows yet",
    )))
}
