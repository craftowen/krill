//! 자리 비운 사이 요약. 앱(메인 창)이 마지막으로 살아 있던 시각([`Layout::last_seen_unix_ns`]) 뒤에
//! 상태가 바뀐 세션을 krilld 상태 목록에서 세어 한 줄로 보인다.
//!
//! `AgentStatus::since_unix_ns`의 의미(krilld `status.rs`):
//! - 상태가 바뀔 때만 갱신된다(`Record::set`). hook 소스면 hook 프로세스 시작 시각이고, `Done` hook은 같은 상태여도
//!   매번 갱신된다. 제목·화면 추정 소스는 krilld가 그 근거를 본 시각이다.
//! - 상태 저장소는 `<상태 디렉터리>/agent-state`에 영속되어 krilld가 재시작해도 이어진다(`Statuses::open`).
//!   다만 파일을 읽지 못하면(버전 변경·손상) 빈 저장소로 시작해 `since`가 0이 되므로 세지 않는다.
//!
//! 한계: 확정 근거인 hook 소스만 센다. 추정 소스는 krilld가 다시 관찰한 시각일 수 있어(재시작 뒤 화면 재판정 등)
//! 앱이 꺼진 동안의 변화라고 단정할 수 없다. 그래서 hook이 없는 에이전트의 변화는 요약에 빠진다.
//! 강제 종료(kill -9)는 종료 시각을 쓰지 못해 마지막 주기 저장 시각([`HEARTBEAT`])을 쓴다.
//!
//! [`Layout::last_seen_unix_ns`]: krill_app::layout::Layout::last_seen_unix_ns

use std::io::{self, Write as _};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use krill_app::hosts::HostTarget;
use krill_app::i18n::catalog::shell as text;
use krill_proto::control::{AgentState, AgentStatus, FEAT_CONTROL, Reply, Request, Source};
use krill_proto::daemon;
use krill_proto::ipc::{self, ClientRole};
use krill_proto::screen::Msg as _;

/// 앱이 살아 있는 동안 마지막 생존 시각을 레이아웃에 다시 쓰는 간격.
pub(super) const HEARTBEAT: Duration = Duration::from_mins(1);
/// 이보다 짧게 비웠으면 요약하지 않는다.
const MIN_AWAY: Duration = Duration::from_mins(1);

pub(super) fn now_ns() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_nanos()).unwrap_or(u64::MAX))
}

/// krilld의 세션별 에이전트 상태(`Request::States`). 데몬을 띄우지 않는다.
pub(super) fn fetch_statuses(target: &HostTarget) -> io::Result<Vec<AgentStatus>> {
    let hello = daemon::client_hello(ClientRole::App, "krill-app-away");
    let conn = target.connect(&hello).map_err(io::Error::other)?;
    if conn.negotiated.features & FEAT_CONTROL == 0 {
        return Err(io::ErrorKind::Unsupported.into());
    }
    let mut sock = conn.stream;
    sock.set_read_timeout(Some(Duration::from_secs(5)))?;
    sock.set_write_timeout(Some(Duration::from_secs(5)))?;
    sock.write_all(&Request::States.encode()?)?;
    let (kind, bytes) = ipc::read_frame(&mut sock)?.ok_or(io::ErrorKind::UnexpectedEof)?;
    match Reply::decode(kind, &bytes)? {
        Reply::States(states) => Ok(states.into_iter().map(|s| s.status).collect()),
        _ => Err(io::ErrorKind::InvalidData.into()),
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(super) struct Counts {
    pub done: usize,
    pub needs_you: usize,
}

/// 비운 시간과 그사이 hook으로 확인된 완료·확인 필요 세션 수. 시각을 모르거나 1분 미만·0개면 `None`.
pub(super) fn count<'a>(
    statuses: impl IntoIterator<Item = &'a AgentStatus>,
    last_seen_ns: Option<u64>,
    now_ns: u64,
) -> Option<(Duration, Counts)> {
    let last_seen = last_seen_ns?;
    let away = Duration::from_nanos(now_ns.checked_sub(last_seen)?);
    if away < MIN_AWAY {
        return None;
    }
    let mut counts = Counts::default();
    for s in statuses {
        if s.source != Some(Source::Hook) || s.since_unix_ns <= last_seen {
            continue;
        }
        match s.state {
            AgentState::Done => counts.done += 1,
            AgentState::Waiting | AgentState::Blocked => counts.needs_you += 1,
            AgentState::Working | AgentState::Exited | AgentState::Unknown => {}
        }
    }
    (counts != Counts::default()).then_some((away, counts))
}

/// 분·시간·일 중 하나로 반올림한 기간.
pub(super) fn duration_text(away: Duration) -> String {
    let mins = (away.as_secs() + 30) / 60;
    if mins < 60 {
        return text::AWAY_MINUTES.format(&[("n", &mins.to_string())]);
    }
    let hours = (mins + 30) / 60;
    if hours < 24 {
        return text::AWAY_HOURS.format(&[("n", &hours.to_string())]);
    }
    text::AWAY_DAYS.format(&[("n", &((hours + 12) / 24).to_string())])
}

/// 예: "Krill을 닫아 둔 2시간 동안 3개 완료 · 1개 확인 필요".
pub(super) fn summary(away: Duration, counts: Counts) -> String {
    let mut parts = Vec::new();
    if counts.done > 0 {
        parts.push(text::AWAY_DONE.format(&[("n", &counts.done.to_string())]));
    }
    if counts.needs_you > 0 {
        parts.push(text::AWAY_NEEDS_YOU.format(&[("n", &counts.needs_you.to_string())]));
    }
    text::AWAY_SUMMARY.format(&[
        ("duration", &duration_text(away)),
        ("counts", &parts.join(" · ")),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIN: u64 = 60 * 1_000_000_000;

    fn hook(state: AgentState, since: u64) -> AgentStatus {
        AgentStatus {
            state,
            source: Some(Source::Hook),
            since_unix_ns: since,
            ..AgentStatus::UNKNOWN
        }
    }

    #[test]
    fn no_time_or_short_absence_is_no_summary() {
        let s = [hook(AgentState::Done, 10 * MIN)];
        assert_eq!(count(&s, None, 100 * MIN), None);
        assert_eq!(count(&s, Some(5 * MIN), 5 * MIN + MIN - 1), None);
        assert_eq!(count(&s, Some(5 * MIN), 4 * MIN), None);
    }

    #[test]
    fn counts_only_hook_changes_after_last_seen() {
        let last = 10 * MIN;
        let mut screen = hook(AgentState::Done, 20 * MIN);
        screen.source = Some(Source::Screen);
        let s = [
            hook(AgentState::Done, 20 * MIN),
            hook(AgentState::Done, 30 * MIN),
            hook(AgentState::Waiting, 40 * MIN),
            hook(AgentState::Blocked, 50 * MIN),
            hook(AgentState::Done, last),        // 꺼지기 전에 이미 완료
            hook(AgentState::Working, 20 * MIN), // 작업 중은 세지 않는다
            hook(AgentState::Done, 0),           // 시각 모름
            screen,                              // 추정 소스
        ];
        let (away, counts) = count(&s, Some(last), 130 * MIN).unwrap();
        assert_eq!(away, Duration::from_hours(2));
        assert_eq!(
            counts,
            Counts {
                done: 2,
                needs_you: 2
            }
        );
        assert_eq!(count(&s[4..], Some(last), 130 * MIN), None, "0개면 없음");
    }

    #[test]
    fn summary_text_rounds_duration() {
        use krill_app::i18n::Language;
        let ko = |m: krill_app::i18n::Message| m.in_language(Language::Ko);
        assert!(ko(text::AWAY_SUMMARY).contains("{duration}"));
        // 언어 전역을 바꾸지 않는 기본(ko)으로 확인한다.
        let s = summary(
            Duration::from_mins(140),
            Counts {
                done: 3,
                needs_you: 1,
            },
        );
        assert_eq!(s, "Krill을 닫아 둔 2시간 동안 3개 완료 · 1개 확인 필요");
        let only_done = summary(
            Duration::from_secs(90),
            Counts {
                done: 1,
                needs_you: 0,
            },
        );
        assert_eq!(only_done, "Krill을 닫아 둔 2분 동안 1개 완료");
        assert_eq!(duration_text(Duration::from_secs(3580)), "1시간");
        assert_eq!(duration_text(Duration::from_mins(1420)), "1일");
        assert_eq!(duration_text(Duration::from_hours(73)), "3일");
    }
}
