//! OWE-534: 칸 크기 → PTY 크기.

use std::collections::HashMap;
use std::time::Duration;

use gpui::{Context, Task, canvas, prelude::*};
use krill_app::layout::PaneId;
use krill_app::terminal::{cell_size, grid};

use super::Shell;

#[derive(Default)]
pub(super) struct Pending(HashMap<PaneId, ((u16, u16), Task<()>)>);

impl Shell {
    pub(super) fn schedule_fit(&mut self, pane: PaneId, want: (u16, u16), cx: &mut Context<Self>) {
        let Some(live) = self.live.get(&pane) else {
            return;
        };
        if live.pane.grid() == Some(want) {
            self.fits.0.remove(&pane);
            return;
        }
        if self
            .fits
            .0
            .get(&pane)
            .is_some_and(|(grid, _)| *grid == want)
        {
            return;
        }
        let task = cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(75))
                .await;
            this.update(cx, |this, cx| {
                this.fits.0.remove(&pane);
                if cx.has_active_drag() {
                    return;
                }
                if let Some(live) = this.live.get_mut(&pane) {
                    let notice = live.pane.fit(want.0, want.1);
                    this.show(notice.as_ref(), cx);
                    cx.notify();
                }
            })
            .ok();
        });
        self.fits.0.insert(pane, (want, task));
    }

    /// 칸 몸통을 덮는 빈 요소. 잰 크기가 75ms 동안 바뀌지 않으면 [`Pane::fit`](krill_app::pane::Pane::fit)한다.
    /// 분할선을 끄는 동안은 보내지 않는다(놓으면 gpui가 다시 그린다) — 드래그 중 리사이즈를 놓을 때 하나로 합친다.
    pub(super) fn fit_probe(
        &self,
        pane: PaneId,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement> {
        self.live.get(&pane)?;
        let me = cx.weak_entity();
        let probe = canvas(
            move |bounds, window, cx| {
                let want = grid(bounds.size, cell_size(window, cx));
                if !cx.has_active_drag() {
                    cx.defer(move |cx| {
                        me.update(cx, |t, cx| {
                            t.schedule_fit(pane, want, cx);
                        })
                        .ok();
                    });
                }
            },
            |_, (), _, _| {},
        );
        Some(probe.absolute().size_full())
    }
}
