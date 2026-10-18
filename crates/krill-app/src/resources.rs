//! Resource Manager 경량판(MK-071). 사이드바 아래 접이식 구역: 펼친 동안 [`POLL`]마다 krilld에 측정값
//! ([`krill_proto::resources`])을 묻고 앱·데몬(krilld+holder)·세션별 메모리와 디스크를 보인다. 값은 krilld가 읽은 커널
//! `phys_footprint` 그대로라 `footprint(1)`과 같은 기준이다. 데몬 재시작·세션 종료는 확인 대화상자에서 고른 뒤에만
//! 보낸다. 재시작 뒤 재연결은 셸의 감시 연결이 맡는다(MK-026). [`FEAT_RESOURCES`]를 광고하지 않는 이전 krilld에는
//! 아무 요청도 보내지 않고 "데몬 업데이트 필요"를 보인다.

use std::io::{self, Read, Write};
use std::sync::Arc;
use std::time::Duration;

use gpui::{Context, PromptLevel, Task, Window, div, prelude::*};
use gpui_component::Sizable as _;
use krill_proto::SessionId;
use krill_proto::control::{FEAT_CONTROL, Reply, Request};
use krill_proto::daemon;
use krill_proto::ipc::{self, ClientRole};
use krill_proto::resources::{Action, FEAT_RESOURCES, Resources};
use krill_proto::screen::Msg as _;

/// 펼친 동안의 조회 간격.
const POLL: Duration = Duration::from_secs(2);
const TIMEOUT: Duration = Duration::from_secs(5);

use crate::hosts::HostTarget;
use crate::i18n::catalog::resources as text;

/// 제어 요청 하나. gpui 테스트는 인메모리 구현을 쓴다(CONVENTIONS §6).
pub type Call = Arc<dyn Fn(&Request) -> io::Result<Reply> + Send + Sync>;

fn daemon_call() -> Call {
    daemon_call_for(None)
}

pub fn daemon_call_for(target: Option<HostTarget>) -> Call {
    #[cfg(test)]
    if target.is_none() {
        return Arc::new(|_| {
            Err(io::Error::new(
                io::ErrorKind::NotConnected,
                "daemon disabled in unit tests",
            ))
        });
    }
    Arc::new(move |req| {
        let hello = daemon::client_hello(ClientRole::App, "krill-app-resources");
        let target = match &target {
            Some(t) => t.clone(),
            None => HostTarget::local_dev()?,
        };
        let conn = target.connect(&hello).map_err(io::Error::other)?;
        let mut sock = conn.stream;
        sock.set_read_timeout(Some(TIMEOUT))?;
        sock.set_write_timeout(Some(TIMEOUT))?;
        exchange(conn.negotiated.features, &mut sock, req)
    })
}

/// 협상한 `features`에 [`FEAT_RESOURCES`]가 없으면 아무것도 쓰지 않고 `Unsupported`(이전 krilld는 variant를 모른다).
fn exchange(features: u64, sock: &mut (impl Read + Write), req: &Request) -> io::Result<Reply> {
    let need = FEAT_CONTROL | FEAT_RESOURCES;
    if features & need != need {
        return Err(io::ErrorKind::Unsupported.into());
    }
    sock.write_all(&req.encode()?)?;
    let (kind, bytes) = ipc::read_frame(sock)?.ok_or(io::ErrorKind::UnexpectedEof)?;
    Ok(Reply::decode(kind, &bytes)?)
}

/// 바이트를 `footprint(1)`과 같은 1024 기준 MB로, 소수 한 자리(반올림).
#[must_use]
pub fn mb(bytes: u64) -> String {
    const MIB: u64 = 1 << 20;
    let tenths = bytes.saturating_mul(10).saturating_add(MIB / 2) / MIB;
    format!("{}.{} MB", tenths / 10, tenths % 10)
}

pub struct Panel {
    call: Call,
    open: bool,
    data: Option<Resources>,
    /// 연결한 krilld가 [`FEAT_RESOURCES`]를 광고하지 않는다.
    outdated: bool,
    note: Option<String>,
    poll: Option<Task<()>>,
    /// OWE-652: 바깥 클릭으로 닫힌 시각. 같은 제스처의 헤더 클릭이 다시 열지 않게 한다.
    dismissed_at: Option<std::time::Instant>,
}

impl std::fmt::Debug for Panel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Panel")
            .field("open", &self.open)
            .field("data", &self.data)
            .finish_non_exhaustive()
    }
}

impl Default for Panel {
    fn default() -> Self {
        Self::new(daemon_call())
    }
}

impl Panel {
    #[must_use]
    pub fn new(call: Call) -> Self {
        Self {
            call,
            open: false,
            data: None,
            outdated: false,
            note: None,
            poll: None,
            dismissed_at: None,
        }
    }

    /// 펼치면 조회를 시작하고, 접으면 멈춘다(접힌 동안 데몬에 묻지 않는다).
    pub fn toggle(&mut self, cx: &mut Context<Self>) {
        if self.open {
            self.close(cx);
            return;
        }
        self.open = true;
        self.poll = Some(self.start_poll(cx));
        cx.notify();
    }

    /// OWE-652: 바깥 클릭·다른 팝오버가 열릴 때 닫는다. 폴링도 함께 멈춘다.
    pub fn close(&mut self, cx: &mut Context<Self>) {
        if self.open {
            self.open = false;
            self.poll = None;
            cx.notify();
        }
    }

    /// OWE-652: 팝오버 밖 마우스 눌림에 닫는다. 같은 제스처의 헤더 클릭이
    /// 다시 열지 않게 시각을 남긴다(클릭은 마우스 뗌 뒤에 온다).
    fn on_outside_down(&mut self, cx: &mut Context<Self>) {
        self.close(cx);
        self.dismissed_at = Some(std::time::Instant::now());
    }

    /// OWE-652: 헤더 클릭. 바깥 닫기와 같은 제스처(약 150ms 안)면 닫힌 채로 둔다.
    fn on_toggle_click(&mut self, cx: &mut Context<Self>) {
        let same_gesture = self
            .dismissed_at
            .take()
            .is_some_and(|at| at.elapsed() < std::time::Duration::from_millis(150));
        if !same_gesture {
            self.toggle(cx);
        }
    }

    fn start_poll(&self, cx: &mut Context<Self>) -> Task<()> {
        let call = Arc::clone(&self.call);
        cx.spawn(async move |this, cx| {
            loop {
                let call = Arc::clone(&call);
                let got = cx
                    .background_executor()
                    .spawn(async move { call(&Request::Resources(Action::Query)) })
                    .await;
                let alive = this.update(cx, |this, cx| {
                    this.outdated =
                        matches!(&got, Err(e) if e.kind() == io::ErrorKind::Unsupported);
                    this.data = match got {
                        Ok(Reply::Resources(r)) => Some(r),
                        Ok(_) | Err(_) => None,
                    };
                    cx.notify();
                });
                if alive.is_err() {
                    return;
                }
                cx.background_executor().timer(POLL).await;
            }
        })
    }

    /// 확인 대화상자에서 첫 버튼을 고르면 `req`를 보낸다. 결과는 `note`에 남긴다.
    fn confirm(
        &mut self,
        prompt: (&str, &str, &str),
        req: Request,
        sent: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (title, detail, yes) = prompt;
        let answer = window.prompt(
            PromptLevel::Warning,
            title,
            Some(detail),
            &[yes, text::CANCEL.get()],
            cx,
        );
        let call = Arc::clone(&self.call);
        cx.spawn(async move |this, cx| {
            if answer.await != Ok(0) {
                return;
            }
            let got = cx
                .background_executor()
                .spawn(async move { call(&req) })
                .await;
            this.update(cx, |this, cx| {
                this.note = Some(match got {
                    Ok(Reply::Failed(f)) => f.to_string(),
                    Ok(_) => sent.to_owned(),
                    Err(e) => e.to_string(),
                });
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub fn confirm_restart(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let prompt = (
            text::RESTART_TITLE.get(),
            text::RESTART_DETAIL.get(),
            text::RESTART.get(),
        );
        let req = Request::Resources(Action::RestartDaemon);
        self.confirm(prompt, req, text::RESTART_SENT.get(), window, cx);
    }

    pub fn confirm_kill(
        &mut self,
        session: SessionId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let title = text::KILL_TITLE
            .get()
            .replace("{id}", &session.0.to_string());
        let prompt = (
            title.as_str(),
            text::KILL_DETAIL.get(),
            text::KILL_CONFIRM.get(),
        );
        let req = Request::Close { session };
        self.confirm(prompt, req, text::KILL_SENT.get(), window, cx);
    }
}

fn row(label: impl Into<String>, value: impl Into<String>, p: &crate::ui::Palette) -> gpui::Div {
    div()
        .flex()
        .items_center()
        .justify_between()
        .gap_2()
        .py_0p5()
        .text_size(crate::ui::TEXT_XS)
        .child(
            div()
                .flex_1()
                .truncate()
                .text_color(p.muted_foreground)
                .child(label.into()),
        )
        .child(
            div()
                .font_family(crate::ui::FONT_UI)
                .text_color(p.popover_foreground)
                .child(value.into()),
        )
}

impl Render for Panel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = crate::ui::palette(cx);
        let tooltip_text = match &self.data {
            Some(r) => {
                let daemon = r.daemon.saturating_add(r.holder.unwrap_or(0));
                format!(
                    "{} · {}: {} · {}: {} · {}: {}",
                    text::TITLE.get(),
                    text::TOTAL.get(),
                    mb(r.total()),
                    text::APP.get(),
                    r.client.map_or_else(|| "-".to_owned(), mb),
                    text::DAEMON.get(),
                    mb(daemon),
                )
            }
            None => text::TITLE.get().to_owned(),
        };
        let total_label = self.data.as_ref().map(|r| mb(r.total()));
        let header = div()
            .id("resources-toggle")
            .debug_selector(|| "resources-toggle".into())
            .aria_label(tooltip_text.clone())
            .flex()
            .items_center()
            .gap_1()
            .px_1()
            .py_0p5()
            .rounded(crate::ui::RADIUS_XS)
            .cursor_pointer()
            .text_size(crate::ui::TEXT_XS)
            .text_color(if self.open {
                p.foreground
            } else {
                p.muted_foreground
            })
            .hover(|d| d.bg(p.accent).text_color(p.accent_foreground))
            .child(
                gpui_component::Icon::default()
                    .data(crate::ui::icons::MEMORY_STICK_SVG)
                    .with_size(gpui_component::Size::XSmall),
            )
            .children(total_label)
            .tooltip(move |window, cx| {
                crate::ui::tooltip_view("resources-toggle", tooltip_text.clone(), window, cx)
            })
            .on_click(cx.listener(|this, _, _, cx| this.on_toggle_click(cx)));
        let popover = self.open.then(|| {
            let content = match &self.data {
                None => {
                    let why = if self.outdated {
                        text::OUTDATED.get()
                    } else {
                        text::FAILED.get()
                    };
                    div()
                        .py_1()
                        .text_size(crate::ui::TEXT_XS)
                        .text_color(p.muted_foreground)
                        .child(why)
                }
                Some(r) => {
                    let daemon = r.daemon.saturating_add(r.holder.unwrap_or(0));
                    div()
                        .flex()
                        .flex_col()
                        .gap_0p5()
                        .child(row(
                            text::APP.get(),
                            r.client.map_or_else(|| "-".to_owned(), mb),
                            p,
                        ))
                        .child(row(text::DAEMON.get(), mb(daemon), p))
                        .children(r.sessions.iter().map(|s| {
                            let id = s.session;
                            let value = if s.processes == 0 {
                                text::EXITED.get().to_owned()
                            } else {
                                mb(s.footprint)
                            };
                            row(format!("{} {}", text::SESSION.get(), id.0), value, p).child(
                                div()
                                    .id(("resources-kill", id.0))
                                    .pl_1()
                                    .cursor_pointer()
                                    .text_color(p.destructive)
                                    .child(text::KILL.get())
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.confirm_kill(id, window, cx);
                                    })),
                            )
                        }))
                        .child(row(text::DISK.get(), mb(r.disk), p))
                        .child(
                            div()
                                .mt_1()
                                .pt_1()
                                .border_t_1()
                                .border_color(p.border)
                                .child(row(text::TOTAL.get(), mb(r.total()), p)),
                        )
                }
            };
            let restart = (!self.outdated).then(|| {
                div()
                    .id("resources-restart")
                    .mt_2()
                    .px_2()
                    .py_1()
                    .rounded(crate::ui::RADIUS_XS)
                    .bg(p.secondary)
                    .text_color(p.secondary_foreground)
                    .hover(|d| d.bg(p.accent).text_color(p.accent_foreground))
                    .cursor_pointer()
                    .text_size(crate::ui::TEXT_XS)
                    .child(text::RESTART.get())
                    .on_click(cx.listener(|this, _, window, cx| this.confirm_restart(window, cx)))
            });
            div()
                .id("resources-popover")
                .debug_selector(|| "resources-popover".into())
                .absolute()
                .bottom(gpui::px(26.))
                .right(gpui::px(0.))
                .w(gpui::px(260.))
                .p_3()
                .rounded(crate::ui::RADIUS_MD)
                .bg(p.popover)
                .text_color(p.popover_foreground)
                .border_1()
                .border_color(p.border)
                .shadow_md()
                .flex()
                .flex_col()
                // OWE-652: 바깥 클릭·다른 팝오버가 열리면 닫는다.
                .on_mouse_down_out(cx.listener(|this, _, _, cx| this.on_outside_down(cx)))
                .child(content)
                .children(restart)
                .children(self.note.clone().map(|n| {
                    div()
                        .mt_1()
                        .text_size(crate::ui::TEXT_XS)
                        .text_color(p.muted_foreground)
                        .child(crate::i18n::cached_notice(&n))
                }))
        });
        div()
            .relative()
            .flex()
            .items_center()
            .child(header)
            .children(popover)
    }
}

#[cfg(test)]
mod tests;
