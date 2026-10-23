//! 분리된 세션(OWE-492). 어느 칸에도 걸리지 않은 krilld 세션(`krill`로 만든 세션 등)을
//! 사이드바에 보인다: 누르면 새 탭으로 다시 붙고, ×는 세션을 닫는다. 명시적인 칸·탭 닫기는 세션도 종료한다.
//! 목록은 기존 control `List`에서 레이아웃의 세션을 뺀 것이라 새 프로토콜 번호가 없다.

use std::io::{self, Write};
use std::time::Duration;

use gpui::{Context, EventEmitter, Task, Window, div, prelude::*};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::{Icon, IconName, Sizable as _};
use krill_proto::SessionId;
use krill_proto::control::{FEAT_CONTROL, Info, Reply, Request};
use krill_proto::daemon;
use krill_proto::ipc::{self, ClientRole};
use krill_proto::screen::Msg as _;

const TIMEOUT: Duration = Duration::from_secs(5);

use crate::hosts::HostTarget;
use crate::i18n::catalog::detached as text;

/// `all` 가운데 `attached`(레이아웃의 칸이 가진 세션)에 없는 것. 순서는 `all`을 따른다.
#[must_use]
pub fn detached(all: &[Info], attached: &[SessionId]) -> Vec<Info> {
    let attached: std::collections::HashSet<_> = attached.iter().copied().collect();
    all.iter()
        .filter(|i| !attached.contains(&i.session))
        .copied()
        .collect()
}

/// 제어 요청 하나. 데몬을 띄우지 않는다(데몬 수명은 셸이 맡는다).
fn call(target: &HostTarget, req: &Request) -> io::Result<Reply> {
    let hello = daemon::client_hello(ClientRole::App, "krill-app-detached");
    let conn = target.connect(&hello).map_err(io::Error::other)?;
    if conn.negotiated.features & FEAT_CONTROL == 0 {
        return Err(io::ErrorKind::Unsupported.into());
    }
    let mut sock = conn.stream;
    sock.set_read_timeout(Some(TIMEOUT))?;
    sock.set_write_timeout(Some(TIMEOUT))?;
    sock.write_all(&req.encode()?)?;
    let (kind, bytes) = ipc::read_frame(&mut sock)?.ok_or(io::ErrorKind::UnexpectedEof)?;
    Ok(Reply::decode(kind, &bytes)?)
}

fn list(target: &HostTarget) -> io::Result<Vec<Info>> {
    match call(target, &Request::List)? {
        Reply::Sessions(all) => Ok(all),
        _ => Err(io::ErrorKind::InvalidData.into()),
    }
}

fn close(target: &HostTarget, session: SessionId) -> io::Result<()> {
    match call(target, &Request::Close { session })? {
        Reply::Closed => Ok(()),
        Reply::Failed(f) => Err(io::Error::other(f.to_string())),
        _ => Err(io::ErrorKind::InvalidData.into()),
    }
}

/// 사용자가 분리된 세션을 다시 열려고 눌렀다.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Open(pub SessionId);

/// 사이드바 "분리된 세션" 목록. 셸이 레이아웃·연결이 바뀔 때 [`Panel::refresh`]를 부른다.
#[derive(Debug, Default)]
pub struct Panel {
    items: Vec<Info>,
    /// 마지막 [`Panel::refresh`]의 호스트 타깃. ×가 쓴다.
    target: Option<HostTarget>,
    task: Option<Task<()>>,
    expanded: bool,
    scroll: gpui::UniformListScrollHandle,
    generation: u64,
}

impl EventEmitter<Open> for Panel {}

impl Panel {
    /// krilld 목록을 다시 읽는다. 실패하면(끊김 등) 이전 목록을 둔다.
    // ponytail: 이벤트(레이아웃 변경·연결·칸 연결)에서만 다시 읽는다. `krill`로 만든 세션은 다음 이벤트에 보인다.
    // 즉시 보여야 하면 krilld 세션 변경 통보를 붙인다.
    pub fn refresh(
        &mut self,
        target: impl Into<HostTarget>,
        attached: Vec<SessionId>,
        cx: &mut Context<Self>,
    ) {
        let target = target.into();
        if self.target.as_ref() != Some(&target) {
            self.items.clear();
            cx.notify();
        }
        self.generation = self.generation.wrapping_add(1);
        let generation = self.generation;
        self.target = Some(target.clone());
        self.task = Some(cx.spawn(async move |this, cx| {
            let clean_target = target.clone();
            let got = cx
                .background_executor()
                .spawn(async move { list(&target) })
                .await;
            let Ok(all) = got else { return };
            let mut rest = detached(&all, &attached);
            let exited: Vec<SessionId> = rest
                .iter()
                .filter(|i| i.status.is_some())
                .map(|i| i.session)
                .collect();
            rest.retain(|i| i.status.is_none());
            if !exited.is_empty() {
                cx.background_executor()
                    .spawn(async move {
                        for s in exited {
                            if let Err(e) = close(&clean_target, s) {
                                tracing::debug!(
                                    session = s.0,
                                    ?e,
                                    "prune exited detached session failed"
                                );
                            }
                        }
                    })
                    .detach();
            }
            this.update(cx, |this, cx| {
                if this.generation != generation {
                    return;
                }
                this.items = rest;
                cx.notify();
            })
            .ok();
        }));
    }

    /// 세션을 닫는다. krilld는 끝난 뒤에 목록에서 지우므로 여기서 먼저 뺀다.
    fn close(&mut self, target: HostTarget, session: SessionId, cx: &mut Context<Self>) {
        self.generation = self.generation.wrapping_add(1);
        self.items.retain(|i| i.session != session);
        cx.notify();
        cx.background_executor()
            .spawn(async move {
                if let Err(e) = close(&target, session) {
                    eprintln!("session close failed: {e}");
                }
            })
            .detach();
    }
}

impl Render for Panel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = crate::ui::palette(cx);
        let expanded = self.expanded;
        let any_running = self.items.iter().any(|i| i.status.is_none());
        div().flex().flex_col().when(!self.items.is_empty(), |d| {
            d.border_t_1()
                .border_color(p.sidebar_border)
                .child(
                    div()
                        .id("detached-toggle")
                        .px_3()
                        .h(gpui::px(26.))
                        .cursor_pointer()
                        .flex()
                        .items_center()
                        .justify_between()
                        .text_size(crate::ui::TEXT_2XS)
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(p.muted_foreground.opacity(0.75))
                        .hover(|d| d.text_color(p.sidebar_foreground))
                        .gap_2()
                        .child(
                            div()
                                .flex()
                                .flex_1()
                                .min_w_0()
                                .items_center()
                                .gap_1p5()
                                .child(
                                    Icon::new(if expanded {
                                        IconName::ChevronDown
                                    } else {
                                        IconName::ChevronRight
                                    })
                                    .with_size(crate::ui::ICON_XS)
                                    .text_color(p.muted_foreground.opacity(0.65)),
                                )
                                .child(div().flex_none().child(text::TITLE.get()))
                                // 탭 밖 세션도 krill-holder가 PTY를 쥐고 있다. 종료된 것만 남으면 붙이지 않는다.
                                .when(any_running, |d| {
                                    d.child(
                                        div()
                                            .min_w_0()
                                            .truncate()
                                            .font_weight(gpui::FontWeight::NORMAL)
                                            .child(text::SUBTITLE.get()),
                                    )
                                }),
                        )
                        .child(
                            div()
                                .flex_none()
                                .px_1()
                                .rounded(crate::ui::RADIUS_XS)
                                .bg(p.muted)
                                .font_family(crate::ui::FONT_MONO)
                                .text_size(crate::ui::TEXT_2XS)
                                .child(self.items.len().to_string()),
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.expanded = !this.expanded;
                            cx.notify();
                        })),
                )
                .when(expanded, |d| {
                    d.pb_1().child(
                        gpui::uniform_list(
                            "detached-scroll",
                            self.items.len(),
                            cx.processor(|this, range: std::ops::Range<usize>, _, cx| {
                                let p = crate::ui::palette(cx);
                                range
                                    .filter_map(|index| {
                                        let i = this.items.get(index)?;
                                        let id = i.session;
                                        let generation = this.generation;
                                        let target = this.target.clone();
                                        // 실행 중이면 보존 톤 점, 종료됐으면 흐린 점(같은 폭이라 라벨이 줄 맞춤된다).
                                        let dot = if i.status.is_none() {
                                            p.preserve
                                        } else {
                                            p.muted_foreground.opacity(0.5)
                                        };
                                        let label = match i.status {
                                            Some(_) => text::SESSION_EXITED.format(&[
                                                ("id", &id.0.to_string()),
                                                ("state", text::EXITED.get()),
                                            ]),
                                            None => text::SESSION_PID.format(&[
                                                ("id", &id.0.to_string()),
                                                ("pid", &i.pid.to_string()),
                                            ]),
                                        };
                                        Some(
                                            div()
                                                .debug_selector(move || {
                                                    format!("detached-row-{}", id.0)
                                                })
                                                .h(gpui::px(28.))
                                                .flex()
                                                .items_center()
                                                .gap_1p5()
                                                .mx_1()
                                                .px_2()
                                                .py_1()
                                                .rounded(crate::ui::RADIUS_XS)
                                                .text_size(crate::ui::TEXT_XS)
                                                .text_color(p.sidebar_foreground)
                                                .hover(|d| {
                                                    d.bg(p.sidebar_accent)
                                                        .text_color(p.sidebar_accent_foreground)
                                                })
                                                .child(
                                                    div()
                                                        .size(crate::ui::STATE_DOT)
                                                        .flex_none()
                                                        .rounded_full()
                                                        .bg(dot),
                                                )
                                                .child(
                                                    div()
                                                        .id(("detached-open", id.0))
                                                        .flex_1()
                                                        .min_w_0()
                                                        .truncate()
                                                        .cursor_pointer()
                                                        .child(label)
                                                        .on_click(cx.listener(
                                                            move |this, _, _, cx| {
                                                                if this.generation == generation {
                                                                    cx.emit(Open(id));
                                                                }
                                                            },
                                                        )),
                                                )
                                                .child(
                                                    crate::ui::button_with_tooltip(
                                                        Button::new(("detached-close", id.0))
                                                            .ghost()
                                                            .xsmall()
                                                            .icon(IconName::Close),
                                                        format!("detached-close-{id}", id = id.0),
                                                        text::CLOSE.get(),
                                                    )
                                                    .on_click(cx.listener(move |this, _, _, cx| {
                                                        if this.generation == generation
                                                            && let Some(target) = target.clone()
                                                        {
                                                            this.close(target, id, cx);
                                                        }
                                                    })),
                                                ),
                                        )
                                    })
                                    .collect()
                            }),
                        )
                        .track_scroll(&self.scroll)
                        .h(gpui::px(112.)),
                    )
                })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(id: u64) -> Info {
        Info {
            session: SessionId(id),
            pid: 100,
            status: None,
        }
    }

    #[test]
    fn detached_is_list_minus_layout() {
        let all = [info(1), info(2), info(3)];
        assert_eq!(
            detached(&all, &[SessionId(2)]),
            vec![info(1), info(3)],
            "칸에 걸린 세션만 빠진다"
        );
        assert_eq!(detached(&all, &[]), all.to_vec());
        assert!(detached(&[], &[SessionId(1)]).is_empty());
    }
    #[gpui::test]
    fn detached_list_only_constructs_visible_rows(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);
        let (_, cx) = cx.add_window_view(|_, _| Panel {
            items: (0..10_000).map(info).collect(),
            expanded: true,
            ..Panel::default()
        });
        cx.run_until_parked();
        assert!(cx.debug_bounds("detached-row-0").is_some());
        assert!(cx.debug_bounds("detached-row-9999").is_none());
    }
}
