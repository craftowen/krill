//! holder의 SIGCHLD/Exited는 루트 종료를 맡는다. krilld는 자식이 아닌 전경 pgid 리더를
//! `tcgetpgrp` + `NOTE_EXIT`로 확인한다. pgid 변화(작업 중지·백그라운드 이동)만으로 종료라고 하지 않는다.
//! PTY 읽기 스레드는 건드리지 않고, 데몬 전체 감시 스레드 하나가 그룹과 조용해진 화면을 확인한다.

use std::collections::BTreeMap;
use std::panic::{self, AssertUnwindSafe};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use krill_proto::SessionId;

use super::detect::Detector;
use crate::registry::Registry;
use crate::sys::ExitWatch;

const POLL: Duration = Duration::from_millis(100);

#[derive(Default)]
struct Tracked {
    detector: Detector,
    /// pid 재사용을 구분하는 시작 시각. 이전 그룹의 늦은 종료는 새 그룹을 종료시키지 않는다.
    process: Option<(u32, u64)>,
    /// Name lookups may be reused only after start identity is revalidated each tick.
    name: Option<(u32, u64, Instant, String)>,
    /// 마지막으로 문자열화한 화면(revision·제목·평문). revision이 그대로면 다시 문자열화하지 않고
    /// 이 캐시로 감지한다(OWE-586: 유휴 20세션 문자열화 비용 제거).
    evidence: Option<(u64, String, String)>,
}

pub(crate) fn monitor(reg: &Registry) {
    // 기동 검사가 panic해도 감시 없이 돌지 않게 `None`(감시 없음)으로 떨어진다(K-06).
    let watch = panic::catch_unwind(ExitWatch::new)
        .ok()
        .and_then(std::result::Result::ok);
    let mut tracked: BTreeMap<SessionId, Tracked> = BTreeMap::new();
    loop {
        // 한 바퀴의 panic이 감시 전체를 영구 정지시키지 않는다. 죽으면 GC(디스크 상한)·상태
        // 감지·동면·Exited 처리가 로그 한 줄 외에는 조용히 멈춘다(K-06).
        if panic::catch_unwind(AssertUnwindSafe(|| {
            tick(reg, watch.as_ref(), &mut tracked);
        }))
        .is_err()
        {
            tracing::warn!("agent-status tick panicked; continuing after backoff");
            thread::sleep(POLL * 10);
            continue;
        }
        thread::sleep(POLL);
    }
}

/// 감시 한 바퀴(GC·종료 감지·상태 추론·동면). panic하면 호출자(`monitor`)가 잡고 다음 바퀴에 계속한다.
fn tick(reg: &Registry, watch: Option<&ExitWatch>, tracked: &mut BTreeMap<SessionId, Tracked>) {
    reg.gc_due(Instant::now(), SystemTime::now());
    let exits = watch.and_then(|w| w.drain().ok()).unwrap_or_default();
    for (&id, t) in &mut *tracked {
        if let Some((pid, start)) = t.process
            && exits.contains(&pid)
        {
            if !crate::sys::start_us(pid).is_ok_and(|current| current != start) {
                reg.with_status(id, |s| s.exited(id));
            }
            t.process = None;
        }
    }
    let mut sessions = Vec::new();
    reg.each(|id, root, exited, s| {
        if exited.is_none() && !s.is_closed() {
            sessions.push((id, root, s.tap()));
        }
    });
    tracked.retain(|id, _| sessions.iter().any(|(live, _, _)| id == live));
    // 서명 규칙 파일이 교체되면 다음 폴링부터 새 규칙을 쓴다(재시작 없음, MK-091).
    let rules = reg.screen_rules();
    for (id, root, tap) in sessions {
        let t = tracked.entry(id).or_default();
        if t.detector.rules() != &rules {
            t.detector.set_rules(rules.clone());
        }
        let Some(status) = reg.with_status(id, |s| s.get(id)) else {
            continue;
        };
        if let Some((title, screen, rev)) = tap.evidence_since(t.evidence.as_ref().map(|e| e.0)) {
            t.evidence = Some((rev, title, screen));
        }
        let Some((rev, title, screen)) = t.evidence.as_ref() else {
            continue;
        };
        let evidence = t
            .detector
            .observe(title, screen, *rev, &status, Instant::now());
        let foreground = tap.foreground();
        #[cfg(windows)]
        let foreground = foreground.or_else(|| crate::sys::session_agent_process(root));
        if let Some(pid) = foreground {
            let mut identity = crate::sys::start_us(pid).ok();
            let name = if let Some((cached_pid, cached_start, at, name)) = &t.name
                && *cached_pid == pid
                && identity == Some(*cached_start)
                && at.elapsed() < Duration::from_secs(1)
            {
                name.clone()
            } else {
                let name = crate::sys::process_name(pid).unwrap_or_default();
                if crate::sys::start_us(pid).ok() != identity {
                    identity = None;
                }
                t.name = identity.map(|start| (pid, start, Instant::now(), name.clone()));
                name
            };
            let name = name.to_ascii_lowercase();
            let agent = matches!(
                name.as_str(),
                "claude" | "codex" | "opencode" | "cursor-agent" | "agy" | "antigravity" | "pi"
            ) || (pid != root
                && (matches!(name.as_str(), "node" | "bun") && !status.agent.is_empty()
                    || evidence.is_some_and(|e| e.source == krill_proto::control::Source::Title)
                        && status.source != Some(krill_proto::control::Source::Process)));
            if agent
                && let Some(start) = identity
                && t.process != Some((pid, start))
            {
                // 먼저 등록해 직후 종료도 놓치지 않는다.
                if let Some(w) = watch {
                    match w.add(pid) {
                        Ok(()) => {
                            t.process = Some((pid, start));
                            reg.with_status(id, |s| s.started(id));
                        }
                        Err(e)
                            if e.raw_os_error() == Some(libc::ESRCH)
                                || cfg!(windows) && e.raw_os_error() == Some(87) =>
                        {
                            reg.with_status(id, |s| s.exited(id));
                        }
                        Err(_) => {}
                    }
                }
            }
        }
        if let Some(e) = evidence {
            reg.with_status(id, |s| s.inferred(id, e, super::now_ns()));
        }
    }
    let mut ids = Vec::new();
    reg.each(|id, _, _, _| {
        ids.push(id);
        reg.observe_webhook(id);
    });
    reg.retain_webhooks(&ids);
    reg.hibernate_idle();
}
