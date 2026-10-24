//! 명시적인 칸·탭 닫기만 데몬 세션을 종료한다. 탭 전환·창 종료는 연결만 놓는다.

use krill_proto::control::{Failure, Reply, Request};

use super::*;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Policy {
    Preserve,
    Terminate,
}

/// Create가 UI의 칸 제거보다 늦게 끝나는 경우에도 종료 의도를 전달한다.
#[derive(Default)]
pub(super) struct Creation {
    closed: bool,
    session: Option<krill_proto::SessionId>,
}

impl Creation {
    pub(super) fn publish(state: &std::sync::Mutex<Self>, session: krill_proto::SessionId) -> bool {
        let mut state = state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.closed {
            return false;
        }
        state.session = Some(session);
        true
    }

    fn close(&mut self) -> Option<krill_proto::SessionId> {
        self.closed = true;
        self.session.take()
    }
}

pub(super) fn send(
    call: &krill_app::resources::Call,
    session: krill_proto::SessionId,
) -> std::io::Result<()> {
    match call(&Request::Close { session })? {
        Reply::Closed | Reply::Failed(Failure::NoSession) => Ok(()),
        Reply::Failed(f) => Err(std::io::Error::other(f.to_string())),
        _ => Err(std::io::ErrorKind::InvalidData.into()),
    }
}

impl Shell {
    pub(super) fn close_layout(&mut self, cx: &mut Context<Self>, f: impl FnOnce(&mut Layout)) {
        self.close_with_policy(Policy::Terminate, cx, f);
    }

    /// 보이던 워크스페이스의 탭이 모두 닫히면 다른 워크스페이스로 넘어가지 않고 빈 화면으로 남는다(Orca 패리티).
    pub(super) fn close_with_policy(
        &mut self,
        policy: Policy,
        cx: &mut Context<Self>,
        f: impl FnOnce(&mut Layout),
    ) {
        let shown: Vec<PaneId> = self
            .workspace_tabs(cx)
            .into_iter()
            .flat_map(|index| self.layout.tab_panes(index))
            .collect();
        self.change_with_policy(true, policy, cx, |layout| {
            f(layout);
            if !shown.is_empty() && !shown.iter().any(|pane| layout.pane_exists(*pane)) {
                layout.deactivate();
            }
        });
    }

    /// 미저장 파일 확인이 끝나 실제로 제거된 칸만 종료한다. 같은 세션을 보는 다른 칸은 보존한다.
    pub(super) fn close_removed_sessions(&mut self, before: &Layout, cx: &mut Context<Self>) {
        let mut closing = Vec::new();
        let mut contended = Vec::new();
        for index in 0..before.tabs().len() {
            for pane in before.tab_panes(index) {
                if self.layout.pane_exists(pane) {
                    continue;
                }
                let host = before.host(pane).clone();
                if before
                    .session(pane)
                    .is_some_and(|session| self.layout.sessions_for_host(&host).contains(&session))
                {
                    continue;
                }
                let pending = self
                    .live
                    .get(&pane)
                    .map(|live| &live.creation)
                    .or_else(|| self.creations.get(&pane))
                    .and_then(|creation| match creation.try_lock() {
                        Ok(mut state) => state.close(),
                        Err(std::sync::TryLockError::Poisoned(error)) => error.into_inner().close(),
                        Err(std::sync::TryLockError::WouldBlock) => {
                            contended.push((host.clone(), Arc::clone(creation)));
                            None
                        }
                    });
                for session in before.session(pane).into_iter().chain(pending) {
                    if self.layout.sessions_for_host(&host).contains(&session)
                        || closing.contains(&(host.clone(), session))
                    {
                        continue;
                    }
                    closing.push((host.clone(), session));
                }
            }
        }
        for (host, creation) in contended {
            let call = self
                .target_for_host(&host)
                .map(|target| krill_app::resources::daemon_call_for(Some(target)));
            #[cfg(test)]
            let call = self.close_call.clone().or(call);
            let mut remaining = self.layout.sessions_for_host(&host);
            remaining.extend(
                closing
                    .iter()
                    .filter(|(closing_host, _)| *closing_host == host)
                    .map(|(_, session)| *session),
            );
            cx.spawn(async move |this, cx| {
                let result = cx
                    .background_executor()
                    .spawn(async move {
                        // A create in flight owns this short critical section. Wait
                        // off-thread and retain cleanup even when the view is gone.
                        let session = creation
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .close();
                        if let Some(session) =
                            session.filter(|session| !remaining.contains(session))
                        {
                            let call = call.ok_or_else(|| {
                                std::io::Error::other("daemon unavailable for pending close")
                            })?;
                            send(&call, session)?;
                        }
                        Ok::<_, std::io::Error>(())
                    })
                    .await;
                this.update(cx, |this, cx| {
                    if let Err(error) = result {
                        this.notice = Some(error.to_string());
                    }
                    this.refresh_detached(cx);
                    cx.notify();
                })
                .ok();
            })
            .detach();
        }
        for (host, session) in closing {
            self.close_daemon_session(&host, session, cx);
        }
    }

    pub(super) fn close_daemon_session(
        &mut self,
        host: &krill_app::hosts::HostId,
        session: krill_proto::SessionId,
        cx: &mut Context<Self>,
    ) {
        // Also cover a Create result published before the pane received its ID.
        self.workspace_closing.insert((host.clone(), session));
        let call = self
            .target_for_host(host)
            .map(|target| krill_app::resources::daemon_call_for(Some(target)));
        #[cfg(test)]
        let call = self.close_call.clone().or(call);
        let Some(call) = call else {
            self.notice = Some(text::DAEMON_FAILED.get().to_owned());
            return;
        };
        let host = host.clone();
        cx.spawn(async move |this, cx| {
            if cx.update(|cx| super::session_shown(&host, session, cx)) {
                return;
            }
            let bg = cx.background_executor().clone();
            let timer = bg.clone();
            let result = bg
                .spawn(async move {
                    send(&call, session)?;
                    // Closed는 종료 요청의 수락이다. 실제 목록에서 빠진 뒤 UI를 갱신한다.
                    let deadline = Instant::now() + Duration::from_secs(10);
                    loop {
                        match call(&Request::List)? {
                            Reply::Sessions(sessions)
                                if !sessions.iter().any(|s| s.session == session) =>
                            {
                                return Ok(());
                            }
                            Reply::Sessions(_) => {}
                            Reply::Failed(f) => return Err(std::io::Error::other(f.to_string())),
                            _ => return Err(std::io::ErrorKind::InvalidData.into()),
                        }
                        if Instant::now() >= deadline {
                            return Err(std::io::ErrorKind::TimedOut.into());
                        }
                        timer.timer(Duration::from_millis(100)).await;
                    }
                })
                .await;
            this.update(cx, |this, cx| {
                if let Err(error) = result {
                    this.notice = Some(error.to_string());
                }
                this.refresh_detached(cx);
                this.sidebar.update(cx, sidebar::Sidebar::refresh);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// 세션 종료 후 재연결 루프가 같은 칸에 새 셸을 만들지 않도록 열린 칸도 제거한다.
    pub(super) fn close_sidebar_session(
        &mut self,
        host: &krill_app::hosts::HostId,
        session: krill_proto::SessionId,
        cx: &mut Context<Self>,
    ) {
        if !self.layout.sessions_for_host(host).contains(&session) {
            self.close_daemon_session(host, session, cx);
            return;
        }
        self.close_layout(cx, |layout| {
            let focused = layout.focused();
            for index in (0..layout.tabs().len()).rev() {
                for pane in layout.tab_panes(index) {
                    if layout.host(pane) == host && layout.session(pane) == Some(session) {
                        layout.activate(index);
                        layout.focus(pane);
                        layout.close_pane();
                    }
                }
            }
            if let Some(focused) = focused {
                for index in 0..layout.tabs().len() {
                    if layout.tab_panes(index).contains(&focused) {
                        layout.activate(index);
                        layout.focus(focused);
                        break;
                    }
                }
            }
        });
    }
}

#[cfg(all(test, unix))]
mod tests;
