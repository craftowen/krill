//! OWE-534: 칸 크기 → PTY 크기.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use gpui::{Context, Task, canvas, prelude::*};
use krill_app::layout::PaneId;
use krill_app::terminal::{cell_size, grid};

use super::Shell;

#[derive(Default)]
pub(super) struct Pending {
    pub(super) tasks: HashMap<PaneId, ((u16, u16), Task<()>)>,
    /// 직전 렌더와 이번 렌더에 그린 칸. 직전에 없던 칸이 새로 보인 칸이다([`Pane::show`](krill_app::pane::Pane::show)).
    drawn: HashSet<PaneId>,
    drawing: HashSet<PaneId>,
}

impl Pending {
    pub(super) fn next_frame(&mut self) {
        self.drawn = std::mem::take(&mut self.drawing);
    }
}

impl Shell {
    pub(super) fn schedule_fit(&mut self, pane: PaneId, want: (u16, u16), cx: &mut Context<Self>) {
        let Some(live) = self.live.get_mut(&pane) else {
            return;
        };
        if live.pane.grid() == Some(want) && !live.pane.reclaims(want) {
            self.fits.tasks.remove(&pane);
            return;
        }
        if self
            .fits
            .tasks
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
                this.fits.tasks.remove(&pane);
                if cx.has_active_drag() {
                    this.schedule_fit(pane, want, cx);
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
        self.fits.tasks.insert(pane, (want, task));
    }

    /// 칸 몸통을 덮는 빈 요소. 잰 크기가 75ms 동안 바뀌지 않으면 [`Pane::fit`](krill_app::pane::Pane::fit)한다.
    /// 분할선을 끄는 동안은 보내지 않는다(놓으면 gpui가 다시 그린다) — 드래그 중 리사이즈를 놓을 때 하나로 합친다.
    pub(super) fn fit_probe(
        &mut self,
        pane: PaneId,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement> {
        let live = self.live.get_mut(&pane)?;
        if self.fits.drawing.insert(pane) && !self.fits.drawn.contains(&pane) {
            live.pane.show();
        }
        let me = cx.weak_entity();
        let probe = canvas(
            move |bounds, window, cx| {
                let want = grid(bounds.size, cell_size(window, cx));
                cx.defer(move |cx| {
                    me.update(cx, |t, cx| {
                        t.schedule_fit(pane, want, cx);
                    })
                    .ok();
                });
            },
            |_, (), _, _| {},
        );
        Some(probe.absolute().size_full())
    }
}
