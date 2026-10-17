//! 세션 종료 단계(T2 §3.5, MK-051): SIGHUP → 2초 뒤 SIGTERM → 3초 뒤 SIGKILL. 세션 프로세스 그룹(`killpg`)과
//! 함께 `setsid`·`setpgid`로 그룹을 빠져나간 후손도 libproc로 따라가 하나씩 보낸다.

use std::time::{Duration, Instant};

use rustix::process::{Pid, Signal};

use crate::sys;

/// (시작 뒤 경과, 시그널).
const STAGES: [(Duration, Signal); 3] = [
    (Duration::ZERO, Signal::HUP),
    (Duration::from_secs(2), Signal::TERM),
    (Duration::from_secs(5), Signal::KILL),
];

#[derive(Debug)]
pub(crate) struct Teardown {
    leader: Pid,
    started: Instant,
    /// 다음에 실행할 [`STAGES`] 번호. 끝까지 갔으면 `STAGES.len()`.
    stage: usize,
    /// 지금까지 본 후손(리더 포함)과 시작 시각. 리더가 먼저 죽으면 고아는 launchd 밑으로 옮겨 가
    /// 트리로 다시 찾을 수 없으므로 미리 적어 두고 단계마다 따라간다. 시작 시각은 pid 재사용을 가린다.
    procs: Vec<(Pid, u64)>,
}

impl Teardown {
    /// 첫 단계(SIGHUP)를 보내고 시작한다.
    pub fn start(leader: Pid) -> Self {
        let mut t = Self {
            leader,
            started: Instant::now(),
            stage: 0,
            procs: sys::start_time(leader)
                .map(|st| (leader, st))
                .into_iter()
                .collect(),
        };
        t.step();
        t
    }

    /// 다음 단계 시각. 끝났으면 `None`.
    pub fn due(&self) -> Option<Instant> {
        STAGES.get(self.stage).map(|(at, _)| self.started + *at)
    }

    /// 다음 단계를 실행한다. 보낼 프로세스가 하나도 남지 않았으면 남은 단계를 건너뛴다.
    pub fn step(&mut self) {
        let Some(&(_, sig)) = STAGES.get(self.stage) else {
            return;
        };
        self.stage += 1;
        self.refresh();
        // 리더가 살아 있을 때만 그룹으로 보낸다. 리더가 죽은 뒤 그룹에 남은 프로세스는 `procs`에 있다.
        if self.procs.iter().any(|(p, _)| *p == self.leader) {
            rustix::process::kill_process_group(self.leader, sig).ok();
        }
        for (p, _) in &self.procs {
            rustix::process::kill_process(*p, sig).ok();
        }
        if self.procs.is_empty() {
            self.stage = STAGES.len();
        }
    }

    /// 남은 단계를 건너뛰고 바로 SIGKILL을 보낸다(exec 핸드오프 직전 — 진행 상태는 새 이미지로 넘기지 않는다).
    pub fn finish(&mut self) {
        if self.stage < STAGES.len() {
            self.stage = STAGES.len() - 1;
            self.step();
        }
    }

    /// 죽은 것(시작 시각이 달라진 것 포함)을 빼고, 남은 것들의 자식을 끝까지 더한다.
    fn refresh(&mut self) {
        self.procs.retain(|&(p, st)| sys::start_time(p) == Some(st));
        let mut i = 0;
        while let Some(&(p, _)) = self.procs.get(i) {
            for c in sys::children(p) {
                if self.procs.iter().all(|(q, _)| *q != c)
                    && let Some(st) = sys::start_time(c)
                {
                    self.procs.push((c, st));
                }
            }
            i += 1;
        }
    }
}
