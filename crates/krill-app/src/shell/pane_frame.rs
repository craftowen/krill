use gpui::{
    AnyElement, App, Context, ElementId, Hsla, Pixels, SharedString, Window, div, prelude::*,
};
use krill_app::layout::{self, Axis, Branch, PaneId};

use super::{Drag, DragGhost, Shell, WindowRole};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum WorkspaceStage {
    CardStage,
    Flush,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PaneFocusState {
    Active,
    Retained,
    Inactive,
}

impl PaneFocusState {
    #[must_use]
    pub(super) const fn shows_solid_terminal_cursor(self) -> bool {
        matches!(self, Self::Active)
    }

    #[must_use]
    pub(super) const fn border_color(self, palette: &krill_app::ui::Palette) -> Hsla {
        match self {
            Self::Active => palette.ring,
            Self::Retained | Self::Inactive => palette.border,
        }
    }

    #[must_use]
    pub(super) const fn debug_label(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Retained => "retained",
            Self::Inactive => "inactive",
        }
    }
}

#[must_use]
pub(super) fn render_card_stage(tree: AnyElement) -> AnyElement {
    div()
        .id("pane-card-stage")
        .debug_selector(|| "pane-card-stage".into())
        .size_full()
        .min_w_0()
        .min_h_0()
        .p(krill_app::ui::PANE_GAP)
        .child(tree)
        .into_any_element()
}

/// gpui paints `border_1` after children while `overflow_hidden` clips to an axis-aligned box,
/// so a single rounded card `div` paints `surface_bg` and strokes the 1px focus ring on top.
#[must_use]
pub(super) fn render_pane_card(
    id: PaneId,
    focus: PaneFocusState,
    palette: &krill_app::ui::Palette,
    surface_bg: Hsla,
    content: AnyElement,
) -> AnyElement {
    let pane_num = id.get();
    let label = focus.debug_label();
    div()
        .id(ElementId::Name(format!("pane-card-{pane_num}").into()))
        .debug_selector(move || format!("pane-card-{pane_num}-{label}"))
        .size_full()
        .min_w_0()
        .min_h_0()
        .rounded(krill_app::ui::RADIUS_SM)
        .border_1()
        .border_color(focus.border_color(palette))
        .bg(surface_bg)
        .overflow_hidden()
        .child(content)
        .into_any_element()
}

#[must_use]
pub(super) fn split_drag_ratio(cursor_pos: Pixels, split_start: Pixels, split_span: Pixels) -> f32 {
    let gutter_width = f32::from(krill_app::ui::PANE_GAP);
    let first_card_end = f32::from(cursor_pos) - 0.5 * gutter_width;
    let cards_span = (f32::from(split_span) - gutter_width).max(1.0);
    layout::drag_ratio(first_card_end, f32::from(split_start), cards_span)
}

#[must_use]
pub(super) fn split_gutter(
    mine: Vec<Branch>,
    axis: Axis,
    dragging: bool,
    cx: &mut Context<Shell>,
) -> AnyElement {
    let p = krill_app::ui::palette(cx);
    let id = ElementId::Name(format!("split-{mine:?}").into());
    let selector_mine = mine.clone();
    let group_name = SharedString::from(format!("split-gutter-{mine:?}"));
    let pill_group = group_name.clone();
    let drag = Drag::Split(mine.clone());
    let drag_path = mine.clone();
    let reset_path = mine;
    let weak = cx.weak_entity();

    let pill = div()
        .rounded_full()
        .map(|d| match axis {
            Axis::Horizontal => d
                .w(krill_app::ui::SPLIT_GUTTER_PILL_THICKNESS)
                .h(krill_app::ui::SPLIT_GUTTER_PILL_LENGTH),
            Axis::Vertical => d
                .h(krill_app::ui::SPLIT_GUTTER_PILL_THICKNESS)
                .w(krill_app::ui::SPLIT_GUTTER_PILL_LENGTH),
        })
        .map(|d| {
            if dragging {
                d.bg(p.split_divider_strong)
            } else {
                d.bg(krill_app::ui::transparent())
                    .group_hover(pill_group, |s| s.bg(p.split_divider))
            }
        });

    div()
        .id(id)
        .debug_selector(move || format!("split-{selector_mine:?}"))
        .group(group_name)
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .map(|d| match axis {
            Axis::Horizontal => d.w(krill_app::ui::PANE_GAP).h_full().cursor_col_resize(),
            Axis::Vertical => d
                .h(krill_app::ui::PANE_GAP)
                .w_full()
                .flex_col()
                .cursor_row_resize(),
        })
        .child(pill)
        .on_drag(drag, move |_, _, _, cx| {
            let path = drag_path.clone();
            weak.update(cx, |this, cx| {
                this.active_split_drag = Some(path);
                cx.notify();
            })
            .ok();
            cx.new(|_| DragGhost)
        })
        .on_click(cx.listener(move |this, e: &gpui::ClickEvent, _, cx| {
            if e.click_count() < 2 {
                return;
            }
            let path = reset_path.clone();
            this.change(false, cx, move |l| l.set_ratio(&path, 0.5));
            this.save();
        }))
        .into_any_element()
}

impl Shell {
    #[must_use]
    pub(super) fn workspace_stage(&self) -> WorkspaceStage {
        if self.role == WindowRole::Main && !self.layout.tabs().is_empty() {
            WorkspaceStage::CardStage
        } else {
            WorkspaceStage::Flush
        }
    }

    pub(super) fn flush_pending_pane_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.flush_chat_focus(window, cx);
    }

    #[must_use]
    pub(super) fn has_workspace_focus_blocker(&self, cx: &App) -> bool {
        self.modal.is_some()
            || self.dashboard.is_some()
            || self.diff.read(cx).has_center_diff()
            || self.onboarding.is_some()
            || self.jump.is_some()
            || self.files.has_quick_open()
            || self.tab_rename.is_some()
            || self.add_project.is_some()
            || self.continue_session.is_some()
            || self.confirm_close_tab.is_some()
            || self.pending_running_close.is_some()
            || self.tab_context_menu.is_some()
    }

    #[must_use]
    pub(super) fn pane_focus_state(&self, id: PaneId, window: &Window, cx: &App) -> PaneFocusState {
        if self.layout.focused() != Some(id) {
            return PaneFocusState::Inactive;
        }
        if !window.is_window_active()
            || !self.security.is_window_active()
            || self.has_workspace_focus_blocker(cx)
        {
            return PaneFocusState::Retained;
        }
        let owns_focus = if self.layout.file(id).is_some() {
            if self.file_pane_uses_editor_focus(id) {
                self.file_editor_has_focus(id, window, cx)
            } else {
                self.focus.is_focused(window)
            }
        } else if self.is_pane_chat_view(id, cx) {
            self.chat_pane_has_focus(id, window, cx) || self.focus.is_focused(window)
        } else {
            self.focus.is_focused(window)
        };
        if owns_focus {
            PaneFocusState::Active
        } else {
            PaneFocusState::Retained
        }
    }
}
