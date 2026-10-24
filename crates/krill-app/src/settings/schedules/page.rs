//! In-app automation pages. Design reference: Orca (MIT, Copyright (c) 2026 Lovecast Inc.).
use super::*;

mod detail;
mod list;
mod runs;
use crate::ui::{self, BadgeTone, TEXT_SM, TEXT_XS};
use gpui_component::button::Button;
use gpui_component::input::InputEvent;

fn ghost(selector: impl Into<String>, label: impl Into<SharedString>) -> Button {
    let selector = selector.into();
    layout::ghost(gpui::ElementId::Name(selector.clone().into()), label)
        .debug_selector(move || selector.clone())
}
fn cell(value: impl Into<SharedString>, width: f32) -> Div {
    div()
        .w(gpui::px(width))
        .flex_none()
        .min_w_0()
        .text_size(TEXT_SM)
        .child(value.into())
}
fn run_number(run: &ScheduleRun) -> String {
    run.run_number
        .map_or_else(|| "—".to_owned(), |n| format!("#{n}"))
}
fn trigger(run: &ScheduleRun) -> &'static str {
    if run.trigger.as_deref() == Some("manual") {
        text::AUTOMATIONS_MANUAL.get()
    } else {
        text::AUTOMATIONS_SCHEDULED.get()
    }
}
fn schedule_summary(entry: &Schedule) -> String {
    krill_proto::schedule::format_schedule_summary(
        &entry.cron,
        crate::i18n::language() == crate::i18n::Language::Ko,
    )
}
fn status_badge(entry: &Schedule, p: &ui::Palette) -> Div {
    ui::badge(
        p,
        if entry.enabled {
            BadgeTone::Success
        } else {
            BadgeTone::Muted
        },
        if entry.enabled {
            text::AUTOMATIONS_ENABLED.get()
        } else {
            text::AUTOMATIONS_PAUSED.get()
        },
    )
}

impl SettingsView {
    fn automation_navigate(&mut self, view: AutomationsSubView, cx: &mut Context<Self>) {
        if self.automations.sub_view != view {
            self.close_schedule_run_log(cx);
            self.automations.prompt_expanded = false;
            self.disarm(cx);
        }
        self.automations.sub_view = view;
        cx.notify();
    }
    fn automation_open_run(&mut self, run: &ScheduleRun, cx: &mut Context<Self>) {
        self.automations.run_origin = self.automations.sub_view.clone();
        self.automation_navigate(
            AutomationsSubView::RunDetail {
                id: run.id.clone(),
                minute: run.minute,
            },
            cx,
        );
        self.automations.selected_run_number = run.run_number;
        self.open_schedule_run_log(run, cx);
    }
    fn automation_selected_run(&self, id: &str, minute: u64) -> Option<&ScheduleRun> {
        self.services.runs.iter().find(|r| {
            r.id == id
                && r.minute == minute
                && self
                    .automations
                    .selected_run_number
                    .is_none_or(|n| r.run_number == Some(n))
        })
    }
    fn automation_back(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let view = match &self.automations.sub_view {
            AutomationsSubView::List => {
                super::super::close_settings_page_for(Some(cx.entity_id()), window, cx);
                return;
            }
            AutomationsSubView::RunDetail { .. } => self.automations.run_origin.clone(),
            _ => AutomationsSubView::List,
        };
        self.automation_navigate(view, cx);
    }
    fn automation_search(
        &mut self,
        runs: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::Entity<InputState> {
        let slot = if runs {
            &mut self.automations.runs_search_input
        } else {
            &mut self.automations.search_input
        };
        if let Some(input) = slot {
            return input.clone();
        }
        let input = cx.new(|cx| {
            InputState::new(window, cx).placeholder(if runs {
                text::AUTOMATIONS_RUNS_SEARCH.get()
            } else {
                text::AUTOMATIONS_SEARCH.get()
            })
        });
        let subscription = cx.subscribe(&input, |_, _, _: &InputEvent, cx| cx.notify());
        *slot = Some(input.clone());
        self.automations.subscriptions.push(subscription);
        input
    }
    pub(in crate::settings) fn render_automations_page(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let p = ui::palette(cx);
        let view = self.automations.sub_view.clone();
        let mut crumbs = div().flex_1().min_w_0().flex().items_center().gap_2();
        if view == AutomationsSubView::List {
            crumbs = crumbs.child(
                div()
                    .text_size(ui::TEXT_LG)
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(text::AUTOMATIONS_TITLE.get()),
            );
        } else {
            crumbs = crumbs
                .child(
                    ghost("automations-breadcrumb-root", text::AUTOMATIONS_TITLE.get()).on_click(
                        cx.listener(|this, _, _, cx| {
                            this.automation_navigate(AutomationsSubView::List, cx);
                        }),
                    ),
                )
                .child("/");
            match &view {
                AutomationsSubView::Detail { id, .. } => {
                    let name = self
                        .services
                        .schedules
                        .iter()
                        .find(|e| &e.id == id)
                        .map_or(id.as_str(), Schedule::display_name);
                    crumbs = crumbs.child(div().min_w_0().truncate().child(name.to_owned()));
                }
                AutomationsSubView::RunsDashboard => {
                    crumbs = crumbs.child(text::AUTOMATIONS_RUNS.get());
                }
                AutomationsSubView::RunDetail { id, minute } => {
                    let name = self
                        .services
                        .schedules
                        .iter()
                        .find(|e| &e.id == id)
                        .map_or(id.as_str(), Schedule::display_name)
                        .to_owned();
                    let id = id.clone();
                    let number = self
                        .automation_selected_run(&id, *minute)
                        .map(run_number)
                        .unwrap_or_default();
                    crumbs = crumbs
                        .child(ghost("automations-breadcrumb-schedule", name).on_click(
                            cx.listener(move |this, _, _, cx| {
                                this.automation_navigate(
                                    AutomationsSubView::Detail {
                                        id: id.clone(),
                                        tab: DetailTab::Overview,
                                    },
                                    cx,
                                );
                            }),
                        ))
                        .child("/")
                        .child(
                            div()
                                .truncate()
                                .child(format!("{number} · {}", format_run_minute(*minute))),
                        );
                }
                AutomationsSubView::List => {}
            }
        }
        let header = div()
            .id("automations-topbar")
            .debug_selector(|| "automations-topbar".into())
            .w_full()
            .min_w_0()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_2()
            .p_4()
            .border_b_1()
            .border_color(p.border)
            .child(crumbs)
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap_2()
                    .child(
                        ghost(
                            "automations-open-runs-dashboard",
                            text::AUTOMATIONS_RUNS.get(),
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.automation_navigate(AutomationsSubView::RunsDashboard, cx);
                        })),
                    )
                    .child(
                        ghost("automations-refresh", text::AUTOMATIONS_REFRESH.get()).on_click(
                            cx.listener(|this, _, _, cx| {
                                this.services_checked = None;
                                this.service_stamp = None;
                                this.refresh_service_cache(cx);
                            }),
                        ),
                    )
                    .child(
                        layout::primary("automations-new-btn", text::AUTOMATIONS_NEW.get())
                            .debug_selector(|| "automations-new-btn".into())
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.open_schedule_modal(None, window, cx);
                            })),
                    )
                    .child(
                        ghost("automations-page-close", "×")
                            .accessibility_label(text::AUTOMATIONS_CLOSE.get())
                            .on_click(cx.listener(|_, _, window, cx| {
                                super::super::close_settings_page_for(
                                    Some(cx.entity_id()),
                                    window,
                                    cx,
                                );
                            })),
                    ),
            );
        let body = match view {
            AutomationsSubView::List => self.render_automations_list(window, cx),
            AutomationsSubView::Detail { id, tab } => self.render_automation_detail(&id, tab, cx),
            AutomationsSubView::RunsDashboard => self.render_automations_runs(window, cx),
            AutomationsSubView::RunDetail { id, minute } => {
                self.render_automation_run_detail(&id, minute, cx)
            }
        };
        div()
            .id("automations-page")
            .debug_selector(|| "automations-page".into())
            .track_focus(&self.focus)
            .key_context(super::super::SEARCH_CONTEXT)
            .on_action(
                cx.listener(|this, _: &super::super::FocusSearch, window, cx| {
                    let runs = this.automations.sub_view == AutomationsSubView::RunsDashboard;
                    let input = this.automation_search(runs, window, cx);
                    input.update(cx, |s, cx| s.focus(window, cx));
                }),
            )
            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, window, cx| {
                if event.keystroke.key == "escape" {
                    if this.schedule_modal.is_some() {
                        this.close_schedule_modal(cx);
                        window.focus(&this.focus, cx);
                    } else if this.armed.is_some() {
                        this.disarm(cx);
                    } else {
                        this.automation_back(window, cx);
                    }
                    cx.stop_propagation();
                }
            }))
            .size_full()
            .min_w_0()
            .min_h_0()
            .flex()
            .flex_col()
            .bg(p.background)
            .text_color(p.foreground)
            .text_size(TEXT_SM)
            .child(header)
            .child(
                div()
                    .id("automations-body-scroll")
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .overflow_y_scroll()
                    .p_4()
                    .child(body),
            )
            .children(
                self.schedule_error
                    .as_ref()
                    .map(|error| ui::error_banner(p, error.clone())),
            )
            .children(self.render_schedule_modal(window, cx))
    }
}
