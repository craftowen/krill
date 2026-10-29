//! MK-081: opt-in 자동 종료. MK-052 의존은 재개 표식과 앱의 한 경계에 격리한다.
//! Orca 문서의 조건을 독립 구현했다. 소스 코드는 차용하지 않았다.
use krill_proto::os::fs::OpenOptionsExt as _;
use std::collections::{BTreeMap, HashMap};
use std::fs::{self, OpenOptions};
use std::io;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use krill_proto::SessionId;
use krill_proto::control::{AgentState, AgentStatus, Confidence, Source};
use krill_proto::holder::ToHolder;

use super::{Registry, lock};

const IDLE: Duration = Duration::from_mins(30);
const POLL: Duration = Duration::from_secs(1);

#[derive(Default)]
pub(super) struct Clock {
    last: Option<Instant>,
    done: HashMap<SessionId, (u64, Instant)>,
    woken: HashMap<SessionId, SessionId>,
}

impl Clock {
    fn ready(&mut self, id: SessionId, status: &AgentStatus, now: Instant, idle: Duration) -> bool {
        if status.state != AgentState::Done
            || status.source != Some(Source::Hook)
            || status.confidence != Confidence::High
            || status.since_unix_ns == 0
        {
            self.done.remove(&id);
            return false;
        }
        let entry = self.done.entry(id).or_insert((status.since_unix_ns, now));
        if entry.0 != status.since_unix_ns {
            *entry = (status.since_unix_ns, now);
        }
        now.saturating_duration_since(entry.1) >= idle
    }
}

/// `(pid, 부모)` 목록에서 `root` 아래가 비었거나, 자식 하나(에이전트)뿐이고 그 밑이 비었는지 본다.
fn idle_roster(root: u32, links: &[(u32, u32)]) -> bool {
    let mut children = links.iter().filter(|(_, parent)| *parent == root);
    match (children.next(), children.next()) {
        (None, _) => true,
        (Some(&(agent, _)), None) => !links.iter().any(|(_, parent)| *parent == agent),
        _ => false,
    }
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
        // 타이머와 자동 재개를 직렬화한다. 동시에 두 창이 열려도 한 번만 재개한다.
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

    pub(crate) fn hibernate_idle(&self) {
        let mut clock = lock(&self.hibernation);
        let now = Instant::now();
        if clock
            .last
            .is_some_and(|last| now.duration_since(last) < POLL)
        {
            return;
        }
        clock.last = Some(now);
        if !self
            .dir
            .as_ref()
            .is_some_and(|dir| dir.join("hibernation.enabled").is_file())
        {
            clock.done.clear();
            return;
        }
        // 업그레이드(exec 핸드오프) 중에는 holder 송신이 `out` 잠금에 막혀 오래 걸린다.
        // 그 동안 동면이 잠금을 쥐고 기다리지 않게 건너뛴다(K-08).
        if *lock(&self.upgrading) {
            return;
        }
        self.hibernate_at(&mut clock, now, IDLE);
    }

    /// 유휴 묶음을 골라 holder에 종료를 요청한다. 전역 `state` 잠금은 후보 복사·최종
    /// 확인 때만 잠깐 쥐고, `current_directory`·roster 조회·마커 fsync·holder 송신은
    /// 잠금 밖에서 한다. 그 동안 해당 세션 PTY 리더와 레지스트리 전체가 멈추지 않는다(K-08).
    /// 종료 직전 묶음 전체의 guard를 모아 안전성을 마지막으로 확인한다.
    fn hibernate_at(&self, clock: &mut Clock, now: Instant, idle: Duration) {
        let Some(holder) = self
            .holder
            .as_ref()
            .filter(|h| h.version() >= super::TERMINATE_SINCE)
        else {
            return;
        };
        {
            let st = lock(&self.state);
            clock
                .done
                .retain(|id, _| st.sessions.contains_key(id) && !st.exited.contains_key(id));
        }
        // 후보는 `state` 잠금 안에서 복사만 하고 바로 놓는다. 이후의 시스템 조회·IO는 잠금 밖이다(K-08).
        let candidates: Vec<_> = {
            let st = lock(&self.state);
            st.sessions
                .iter()
                .filter(|(id, _)| !st.exited.contains_key(id))
                .map(|(&id, entry)| (id, entry.session.tap(), entry.pid))
                .collect()
        };
        let mut groups = BTreeMap::new();
        for (id, tap, pid) in candidates {
            // 런타임 cwd를 확인하지 못하면 그 세션은 묶음에서 뺀다(종료하지 않는다).
            // 예전에는 여기서 전체 검사를 중단했다.
            let Some(cwd) = tap.cwd(pid) else {
                continue;
            };
            groups
                .entry(cwd)
                .or_insert_with(Vec::new)
                .push((id, tap, pid));
        }
        for group in groups.values() {
            let statuses: Vec<_> = group
                .iter()
                .map(|(id, _, _)| self.status.get(*id))
                .collect();
            // 단락 평가로 뒤쪽 pane의 완료 시계를 누락하지 않는다.
            let ready: Vec<_> = group
                .iter()
                .zip(&statuses)
                .map(|((id, _, _), status)| clock.ready(*id, status, now, idle))
                .collect();
            if ready.iter().any(|&r| !r) {
                continue;
            }
            if !group.iter().all(|(id, _, _)| self.resumable(*id)) {
                continue;
            }
            // 보수적 roster 조건: 에이전트 밑에 자식 프로세스가 하나라도 있거나 전체 조회가 실패하면 종료하지 않는다.
            // 루트는 셸 복귀 래퍼(로그인 셸)이고 에이전트는 그 자식 하나다(`agent::PREFLIGHT`).
            let Ok(pids) = crate::sys::all_pids() else {
                continue;
            };
            let links: Vec<_> = pids
                .into_iter()
                .filter(|&pid| pid > 1)
                .filter_map(|pid| crate::sys::parent_pid(pid).ok().map(|parent| (pid, parent)))
                .collect();
            if group.iter().any(|(_, _, root)| !idle_roster(*root, &links)) {
                continue;
            }
            let guards: Option<Vec<_>> = group
                .iter()
                .zip(&statuses)
                .map(|((_, tap, _), s)| tap.hibernation_guard(s.since_unix_ns, idle))
                .collect();
            let Some(mut guards) = guards else { continue };
            // 묶음 전체의 guard를 보유해 모든 pane의 안전성을 먼저 확인한다.
            // guard를 쥔 뒤 `state`를 다시 보고 그 사이 닫힘·종료를 확인한다(K-08).
            if !self.hibernation_group_live(group.iter().map(|(id, _, _)| *id)) {
                continue;
            }
            let marked: io::Result<Vec<_>> = group
                .iter()
                .map(|(id, _, _)| {
                    let path = self.sleep_path(*id).ok_or(io::ErrorKind::NotFound)?;
                    fs::create_dir_all(path.parent().ok_or(io::ErrorKind::NotFound)?)?;
                    OpenOptions::new()
                        .write(true)
                        .create(true)
                        .truncate(true)
                        .mode(0o600)
                        .open(&path)?
                        .sync_all()?;
                    Ok(path)
                })
                .collect();
            if marked.is_err() {
                for (id, _, _) in group {
                    self.clear_sleeping(*id);
                }
                continue;
            }
            for ((id, _, _), guard) in group.iter().zip(&mut guards) {
                // 응답을 기다리면 on_exited가 state 잠금을 기다린다. 알림만 보내고 fd는 보존한다.
                // `state` 잠금은 쥐지 않았으므로 여기서 기다려도 교착은 없다.
                // 송신에 실패하면 표시만 남기고 세션을 잠그지 않는다(K-22).
                if holder
                    .notify(|req| ToHolder::Terminate { req, id: *id })
                    .is_err()
                {
                    self.clear_sleeping(*id);
                    continue;
                }
                guard.sleeping();
            }
        }
    }

    fn hibernation_group_live(&self, ids: impl IntoIterator<Item = SessionId>) -> bool {
        // 세션 live guard를 이미 쥐고 있다. hook의 state → live와 역순이므로
        // 기다리면 교착한다. 경쟁 중이면 이번 동면을 취소하고 다음 tick에 재검사한다.
        let st = match self.state.try_lock() {
            Ok(st) => st,
            Err(std::sync::TryLockError::Poisoned(error)) => error.into_inner(),
            Err(std::sync::TryLockError::WouldBlock) => return false,
        };
        ids.into_iter().all(|id| {
            st.sessions.contains_key(&id)
                && !st.exited.contains_key(&id)
                && !st.closing.contains(&id)
        })
    }

    fn resumable(&self, id: SessionId) -> bool {
        let Ok(Some(stored)) = self.agents.stored(id) else {
            return false;
        };
        if self.agents.hibernation_blocked(id)
            || stored.hibernation_blocked
            || !stored
                .session_id
                .as_deref()
                .is_some_and(crate::agent::control::valid_session_id)
        {
            return false;
        }
        let Ok(agents) = self.agents.agents() else {
            return false;
        };
        let Some(profile) = agents.profiles.get(&stored.profile) else {
            return false;
        };
        matches!(
            profile
                .program()
                .and_then(|p| std::path::Path::new(p).file_name())
                .and_then(|s| s.to_str()),
            Some("claude" | "codex")
        )
    }
}

#[cfg(test)]
#[cfg(unix)]
mod tests;
