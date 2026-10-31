//! Keep daemon failure identity without changing retry policy or exposing request contents.

use std::fmt;
use std::io;

use krill_proto::control::Failure;

use super::{SessionGone, UnknownOutcome};

#[derive(Debug)]
struct DaemonFailure(Failure);

impl fmt::Display for DaemonFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl std::error::Error for DaemonFailure {}

pub(super) fn failure_error(kind: io::ErrorKind, failure: Failure) -> io::Error {
    io::Error::new(kind, DaemonFailure(failure))
}

#[derive(Debug, PartialEq, Eq)]
struct Details {
    code: &'static str,
    errno: Option<i32>,
}

pub(super) fn control_failure(error: &io::Error) -> Option<Failure> {
    error.get_ref().and_then(|inner| {
        if inner.is::<SessionGone>() {
            Some(Failure::NoSession)
        } else {
            inner.downcast_ref::<DaemonFailure>().map(|e| e.0)
        }
    })
}

fn details(error: &io::Error) -> Details {
    if let Some(unknown) = error
        .get_ref()
        .and_then(|inner| inner.downcast_ref::<UnknownOutcome>())
    {
        return details(&unknown.0);
    }
    let failure = control_failure(error);
    // Native OS errors stay native: wrapping them would lose raw_os_error().
    let errno = match failure {
        Some(Failure::Os(errno)) => Some(errno),
        _ => error.raw_os_error(),
    };
    let code = match failure {
        Some(Failure::NoHolder) => "no_holder",
        Some(Failure::SessionLimit) => "session_limit",
        Some(Failure::NoSession) => "no_session",
        Some(Failure::Denied) => "denied",
        Some(Failure::UnknownAgent) => "unknown_agent",
        Some(Failure::BadRequest) => "bad_request",
        Some(Failure::Os(_)) => "os",
        None if errno.is_some() => "os",
        None => "transport",
    };
    Details { code, errno }
}

/// Internal numeric context for a pane's connection attempt.
#[derive(Clone, Copy, Debug)]
pub struct StartAttempt {
    pub pane: u32,
    pub wanted_session: Option<krill_proto::SessionId>,
    pub attempt: u32,
    pub remote: bool,
}

/// Correlate retries using internal numeric IDs; no paths, accounts or provider conversation IDs.
pub fn record_start_failure(
    operation: &'static str,
    error: &io::Error,
    context: Option<StartAttempt>,
) {
    let info = details(error);
    tracing::warn!(
        operation,
        failure = info.code,
        kind = ?error.kind(),
        errno = ?info.errno,
        outcome_unknown = super::is_unknown_outcome(error),
        pane = context.map(|c| c.pane),
        wanted_session = context.and_then(|c| c.wanted_session.map(|s| s.0)),
        attempt = context.map(|c| c.attempt),
        remote = context.map(|c| c.remote),
        "terminal operation failed"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    const FAILURES: [Failure; 7] = [
        Failure::NoHolder,
        Failure::SessionLimit,
        Failure::NoSession,
        Failure::Denied,
        Failure::UnknownAgent,
        Failure::BadRequest,
        Failure::Os(13),
    ];

    #[test]
    fn control_failure_preserves_retry_kind_message_errno_and_session_gone() {
        for failure in FAILURES {
            let error = crate::pane::failure_to_io_error(failure);
            let kind = match failure {
                Failure::NoHolder => io::ErrorKind::Other,
                Failure::Os(errno) => io::Error::from_raw_os_error(errno).kind(),
                _ => io::ErrorKind::InvalidInput,
            };
            assert_eq!(error.kind(), kind, "{failure:?}");
            assert_eq!(error.to_string(), failure.to_string());
            assert_eq!(
                crate::pane::is_session_gone(&error),
                failure == Failure::NoSession
            );
            assert!(!crate::pane::is_unknown_outcome(&error));
            assert_ne!(details(&error).code, "transport");
            assert_eq!(
                error.raw_os_error(),
                if let Failure::Os(errno) = failure {
                    Some(errno)
                } else {
                    None
                }
            );
        }
    }

    #[test]
    fn metadata_failure_keeps_other_kind_and_does_not_remove_a_pane() {
        for failure in FAILURES {
            let error = failure_error(io::ErrorKind::Other, failure);
            assert_eq!(error.kind(), io::ErrorKind::Other);
            assert_eq!(error.to_string(), failure.to_string());
            assert_eq!(error.raw_os_error(), None);
            assert!(!crate::pane::is_session_gone(&error));
            assert!(!crate::pane::is_unknown_outcome(&error));
            assert_ne!(details(&error).code, "transport");
        }
    }

    #[test]
    fn unknown_outcome_keeps_original_os_number_and_direct_marker() {
        let error = crate::pane::unknown_outcome(io::Error::from_raw_os_error(13));
        assert!(crate::pane::is_unknown_outcome(&error));
        assert!(!crate::pane::is_session_gone(&error));
        assert_eq!(
            details(&error),
            Details {
                code: "os",
                errno: Some(13)
            }
        );
    }

    #[test]
    fn diagnostics_omit_untrusted_error_text() {
        let error = io::Error::other("/private/account/token secret prompt");
        assert_eq!(
            details(&error),
            Details {
                code: "transport",
                errno: None
            }
        );
        assert_eq!(
            details(&failure_error(io::ErrorKind::Other, Failure::Os(13))).errno,
            Some(13)
        );
    }
}
