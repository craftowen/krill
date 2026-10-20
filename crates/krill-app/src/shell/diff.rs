//! MK-085: route review comments through the focused agent's existing input journal.

use gpui::{AnyElement, Context, Entity, Subscription, Window};
use krill_app::diff::{Panel, SendComments, input_bytes};
use krill_term::ime::Output;

use super::Shell;

impl Shell {
    /// 가운데 diff를 닫고 키 입력을 터미널로 되돌린다.
    pub(super) fn close_center_diff(&mut self, cx: &mut Context<Self>) {
        self.diff.update(cx, Panel::clear_selection);
        self.files.refocus = true;
    }

    /// 변경 파일을 고르면 diff 본문을 터미널 작업 공간 위에 덮어 그린다.
    pub(super) fn render_center_diff(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if !self.diff.read(cx).has_center_diff() {
            return None;
        }
        self.diff
            .update(cx, |panel, cx| panel.render_center_diff(window, cx))
    }

    pub(super) fn subscribe_diff(panel: &Entity<Panel>, cx: &mut Context<Self>) -> Subscription {
        cx.subscribe(panel, |this, panel, event: &SendComments, cx| {
            let source = event.1;
            let Some(live) = this
                .layout
                .focused()
                .and_then(|focused| this.live.get(&focused))
                .filter(|l| {
                    l.launch.is_some()
                        && l.pane.is_linked()
                        && !l.pane.is_cold()
                        && !l.pane.is_closed()
                })
            else {
                panel.update(cx, |p, cx| p.no_agent(source, cx));
                return;
            };
            let out = Output {
                pty: input_bytes(&event.0, live.pane.modes()),
                discard_ime: false,
            };
            let accepted = this.send(&out, None, cx);
            panel.update(cx, |p, cx| p.comments_sent(source, accepted, cx));
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;

    #[gpui::test]
    fn review_send_without_connected_agent_is_rejected(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let (shell, cx) = cx.add_window_view(|_, cx| Shell::new(None, cx));
        cx.run_until_parked();
        assert!(cx.debug_bounds("comments-no-agent").is_none());
        cx.update(|_, cx| {
            let panel = shell.read(cx).diff.clone();
            panel.update(cx, |_, cx| cx.emit(SendComments::review("review".into())));
        });
        cx.run_until_parked();
        assert!(cx.debug_bounds("comments-no-agent").is_some());
    }

    #[gpui::test]
    fn selected_changed_file_renders_diff_in_center_and_closes(cx: &mut TestAppContext) {
        use gpui::{Modifiers, MouseButton};

        let dir = std::env::temp_dir().join(format!(
            "krill-center-diff-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("hello.rs"), "pub fn hello() -> i32 {\n    42\n}\n").unwrap();

        cx.update(gpui_component::init);
        let (shell, cx) = cx.add_window_view(|_, cx| Shell::new(None, cx));
        cx.run_until_parked();
        assert!(cx.debug_bounds("center-diff-view").is_none());

        let open = |cx: &mut gpui::VisualTestContext| {
            let repo = dir.clone();
            cx.update(|_, cx| {
                let panel = shell.read(cx).diff.clone();
                panel.update(cx, |p, cx| p.open_diff_file(repo, "hello.rs", false, cx));
            });
            cx.run_until_parked();
        };
        open(cx);

        let center = cx
            .debug_bounds("center-diff-view")
            .expect("selected file diff must render in the center workspace");
        let panel = cx
            .debug_bounds("panel-column")
            .expect("right panel column must be rendered");
        assert!(center.origin.x < panel.origin.x, "{center:?} vs {panel:?}");
        assert!(
            center.size.width > panel.size.width,
            "{center:?} vs {panel:?}"
        );
        // 오른쪽 패널에는 diff 본문이 남지 않는다(가운데에만 하나).
        let viewer = cx.debug_bounds("diff-viewer");
        assert!(
            viewer.is_none_or(|v| v.origin.x < panel.origin.x),
            "{viewer:?}"
        );

        // 닫기 버튼으로 닫는다.
        let close = cx
            .debug_bounds("diff-close")
            .expect("center diff header must provide a close button");
        cx.simulate_mouse_down(close.center(), MouseButton::Left, Modifiers::default());
        cx.simulate_mouse_up(close.center(), MouseButton::Left, Modifiers::default());
        cx.run_until_parked();
        assert!(cx.debug_bounds("center-diff-view").is_none());

        // Esc로도 닫히고, 그 Esc는 터미널로 가지 않는다.
        open(cx);
        assert!(cx.debug_bounds("center-diff-view").is_some());
        cx.update(|window, cx| {
            let focus = shell.read(cx).focus.clone();
            window.focus(&focus, cx);
        });
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        assert!(cx.debug_bounds("center-diff-view").is_none());

        std::fs::remove_dir_all(&dir).ok();
    }
}
