//! 세션 데몬 연결 상태와 화면에 보일 문구(MK-026). gpui 없음 — 셸(`main.rs`의 `shell`)이 배너로 그린다.
//! 재연결 정책은 `krill_proto::daemon`(감시 연결·`reconnect_until`), 입력 보류는 `krill_proto::input::Journal`.

use krill_proto::input::{Fate, Notice};

/// 사용자에게 보이는 ko/en 문자열 키(MK-094).
use crate::i18n::catalog::link as text;

/// 입력 알림에 보이는 최대 글자 수.
const PREVIEW_CHARS: usize = 40;

/// 앱과 krilld 사이 감시 연결의 상태.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Link {
    /// 처음 붙는 중(배너 없음).
    Connecting,
    Up,
    /// 붙어 있다가 끊겼다. 조작 없이 다시 붙는 중.
    Down,
    /// 처음부터 붙지 못했다(이유). 계속 다시 시도한다.
    Failed(String),
}

impl Link {
    /// 일시적인 연결 전환은 백그라운드에서 다시 붙고, 설치·권한·프로토콜 오류만 사용자 조치를 요청한다.
    pub fn connect_failure(error: &krill_proto::daemon::Error) -> Self {
        use std::io::ErrorKind;
        match error {
            krill_proto::daemon::Error::LockTimeout => Self::Down,
            krill_proto::daemon::Error::Io(error)
                if matches!(
                    error.kind(),
                    ErrorKind::UnexpectedEof
                        | ErrorKind::ConnectionReset
                        | ErrorKind::ConnectionAborted
                        | ErrorKind::ConnectionRefused
                        | ErrorKind::BrokenPipe
                        | ErrorKind::Interrupted
                        | ErrorKind::TimedOut
                        | ErrorKind::WouldBlock
                ) =>
            {
                Self::Down
            }
            _ => Self::Failed(error.to_string()),
        }
    }

    /// 가운데 열 위 배너. 붙어 있거나 처음 붙는 중이면 없음.
    pub fn banner(&self) -> Option<String> {
        match self {
            Self::Connecting | Self::Up => None,
            Self::Down => Some(text::RECONNECTING.get().to_owned()),
            Self::Failed(err) => Some(format!(
                "{}: {err} — {}",
                text::FAILED.get(),
                text::RETRYING.get()
            )),
        }
    }
}

/// 배너 색조(STYLEGUIDE §3.3). 세션이 안전한 재연결은 `preserve`, 사용자가 알아야 할 실패는 `warning`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BannerTone {
    Preserve,
    Warning,
}

impl Link {
    /// [`Link::banner`]의 색조. 붙어 있다가 끊긴 `Down`만 보존 톤이다(PTY는 krill-holder가 쥐고 있다).
    #[must_use]
    pub const fn banner_tone(&self) -> BannerTone {
        match self {
            Self::Down => BannerTone::Preserve,
            Self::Connecting | Self::Up | Self::Failed(_) => BannerTone::Warning,
        }
    }
}

/// 멈춘 칸 배너(OWE-586). 보이는 칸 중 보낸 입력의 ack가 PROBE 넘게 오지 않은 칸이 있으면 보인다.
/// ack·재연결이 오면 다음 그리기에서 사라진다.
pub fn stalled_banner(stalled: bool) -> Option<String> {
    stalled.then(|| text::STALLED.get().to_owned())
}

/// 입력 알림 한 줄: 처지와 입력 앞부분(줄바꿈은 ⏎, 다른 제어 문자는 ·).
pub fn notice_text(n: &Notice) -> String {
    let label = match n.fate {
        Fate::Uncertain => text::UNCERTAIN.get(),
        Fate::Expired => text::EXPIRED.get(),
        Fate::NewSession => text::NEW_SESSION.get(),
        Fate::SessionClosed => text::SESSION_CLOSED.get(),
        Fate::Full => text::FULL.get(),
        Fate::Unsupported => text::UNSUPPORTED.get(),
        Fate::AccountSwitchFailed => text::ACCOUNT_SWITCH_FAILED.get(),
    };
    let s = String::from_utf8_lossy(&n.bytes);
    let mut preview: String = s
        .chars()
        .take(PREVIEW_CHARS)
        .map(|c| match c {
            '\r' | '\n' => '⏎',
            c if c.is_control() => '·',
            c => c,
        })
        .collect();
    if s.chars().nth(PREVIEW_CHARS).is_some() {
        preview.push('…');
    }
    format!("{label}: {preview}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transient_connect_failures_retry_without_actionable_error() {
        use krill_proto::daemon::Error;
        use std::io::{self, ErrorKind};
        for kind in [
            ErrorKind::UnexpectedEof,
            ErrorKind::ConnectionReset,
            ErrorKind::TimedOut,
        ] {
            assert_eq!(Link::connect_failure(&Error::Io(kind.into())), Link::Down);
        }
        assert_eq!(Link::connect_failure(&Error::LockTimeout), Link::Down);
        for error in [
            Error::Missing("krill-holder".into()),
            Error::Io(io::Error::from(ErrorKind::PermissionDenied)),
            Error::Io(io::Error::from(ErrorKind::InvalidData)),
            Error::Timeout,
        ] {
            assert!(matches!(Link::connect_failure(&error), Link::Failed(_)));
        }
    }

    #[test]
    fn banner_only_when_not_up() {
        assert_eq!(Link::Up.banner(), None);
        assert_eq!(Link::Connecting.banner(), None);
        assert!(Link::Down.banner().unwrap().contains("다시 연결"));
        let f = Link::Failed("socket path too long".into())
            .banner()
            .unwrap();
        assert!(
            f.contains("socket path too long") && f.contains(text::RETRYING.get()),
            "{f}"
        );
    }

    #[test]
    fn reconnect_is_preserve_and_failure_is_warning() {
        assert_eq!(Link::Down.banner_tone(), BannerTone::Preserve);
        assert_eq!(Link::Failed("x".into()).banner_tone(), BannerTone::Warning);
    }

    #[test]
    fn stalled_banner_only_when_stalled() {
        assert_eq!(stalled_banner(false), None);
        assert!(stalled_banner(true).unwrap().contains("멈췄"));
    }

    #[test]
    fn notice_preview() {
        let n = Notice {
            fate: Fate::Uncertain,
            bytes: "git push\r\x1b한글".as_bytes().to_vec(),
        };
        assert_eq!(
            notice_text(&n),
            format!("{}: git push⏎·한글", text::UNCERTAIN.get())
        );
        let long = Notice {
            fate: Fate::Expired,
            bytes: "가".repeat(PREVIEW_CHARS + 1).into_bytes(),
        };
        let t = notice_text(&long);
        assert!(t.ends_with('…'), "{t}");
        assert_eq!(t.chars().filter(|&c| c == '가').count(), PREVIEW_CHARS);
    }
}
