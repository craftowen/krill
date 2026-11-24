// Adapted from Orca (Copyright (c) 2026 Lovecast Inc., MIT License).
//! Orca's "Bot templates" gallery: search and category chips over a grouped list on the left,
//! the picked template's details on the right, "Use template…" opens New bot pre-filled.

use gpui::{
    App, Context, Entity, EventEmitter, FocusHandle, FontWeight, KeyDownEvent, ScrollHandle,
    SharedString, Subscription, Window, div, prelude::*, px,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::{Icon, IconName, Sizable as _};
use krill_proto::bots::{
    BUILTIN_BOT_TEMPLATES, BotTemplate, BotTemplateCategory, BotTemplateEvent, BotTemplateSchedule,
    find_bot_skill,
};

use super::avatar;
use crate::i18n::{
    self, Language, bot_skill_ko, bot_template_event_ko, bot_template_ko, bot_template_schedule_ko,
    catalog::bots as text,
};
use crate::ui;

/// Orca `w-[min(1080px,100vw-2rem)] h-[min(780px,100vh-2rem)]`.
const WIDTH: f32 = 1080.;
const HEIGHT: f32 = 780.;
/// Below this width the details replace the list (Orca's container query).
const SPLIT_MIN_WIDTH: f32 = 896.;

#[derive(Debug, Clone)]
pub enum Event {
    Use(&'static BotTemplate),
    /// "Blank bot": an empty New bot, with the search text as its job when there is one.
    Blank(Option<String>),
    Close,
}

fn is_korean() -> bool {
    i18n::language() == Language::Ko
}

#[must_use]
pub fn localized_template_name(tpl: &BotTemplate) -> &'static str {
    if is_korean()
        && let Some(ko) = bot_template_ko(tpl.id)
    {
        return ko.name;
    }
    tpl.name
}

#[must_use]
pub fn localized_template_description(tpl: &BotTemplate) -> &'static str {
    if is_korean()
        && let Some(ko) = bot_template_ko(tpl.id)
    {
        return ko.description;
    }
    tpl.description
}

#[must_use]
pub fn localized_template_first_chat(tpl: &BotTemplate) -> &'static str {
    if is_korean()
        && let Some(ko) = bot_template_ko(tpl.id)
    {
        return ko.first_chat;
    }
    tpl.first_chat
}

#[must_use]
pub fn localized_template_watches(tpl: &BotTemplate, fallback: Option<String>) -> Option<String> {
    if is_korean()
        && let Some(ko) = bot_template_ko(tpl.id)
    {
        return Some(ko.watches.to_owned());
    }
    fallback
}

#[must_use]
pub fn localized_template_does(tpl: &BotTemplate, fallback: Option<String>) -> Option<String> {
    if is_korean()
        && let Some(ko) = bot_template_ko(tpl.id)
    {
        return Some(ko.does.to_owned());
    }
    fallback
}

#[must_use]
pub fn localized_template_reports(tpl: &BotTemplate, fallback: Option<String>) -> Option<String> {
    if is_korean()
        && let Some(ko) = bot_template_ko(tpl.id)
    {
        return Some(ko.reports.to_owned());
    }
    fallback
}

#[must_use]
pub fn localized_template_needs_setup(tpl: &BotTemplate) -> Option<&'static str> {
    if is_korean()
        && let Some(ko) = bot_template_ko(tpl.id)
    {
        return ko.needs_setup.or(tpl.needs_setup);
    }
    tpl.needs_setup
}

#[must_use]
pub fn localized_template_examples(tpl: &BotTemplate) -> [&'static str; 2] {
    if is_korean()
        && let Some(ko) = bot_template_ko(tpl.id)
    {
        return ko.examples;
    }
    tpl.example
}

#[must_use]
pub fn localized_schedule(
    tpl_id: &str,
    sched: &BotTemplateSchedule,
) -> (&'static str, &'static str) {
    if is_korean()
        && let Some(ko) = bot_template_schedule_ko(tpl_id, sched.id)
    {
        return ko;
    }
    (sched.when, sched.what)
}

#[must_use]
pub fn localized_event_name(tpl_id: &str, ev: &BotTemplateEvent) -> &'static str {
    if is_korean()
        && let Some(ko) = bot_template_event_ko(tpl_id, ev.id)
    {
        return ko;
    }
    ev.name
}

#[must_use]
pub fn localized_skill_title(skill_id: &str) -> String {
    if is_korean()
        && let Some((title, _)) = bot_skill_ko(skill_id)
    {
        return format!("{title} ({skill_id})");
    }
    skill_id.to_owned()
}

#[must_use]
pub fn localized_skill_description(skill_id: &str, fallback: &'static str) -> &'static str {
    if is_korean()
        && let Some((_, desc)) = bot_skill_ko(skill_id)
    {
        return desc;
    }
    fallback
}

/// Templates matching `category` and every word of `query` (name, line, category, connectors,
/// skills, id — both English and Korean), in catalog order.
#[must_use]
pub fn filter_templates(
    category: Option<BotTemplateCategory>,
    query: &str,
) -> Vec<&'static BotTemplate> {
    let q = query.trim().to_lowercase();
    BUILTIN_BOT_TEMPLATES
        .iter()
        .filter(|tpl| category.is_none_or(|c| tpl.category == c))
        .filter(|tpl| {
            if q.is_empty() {
                return true;
            }
            let mut hay = format!(
                "{} {} {} {}",
                tpl.name,
                tpl.description,
                tpl.id,
                category_label(tpl.category)
            );
            if let Some(ko) = bot_template_ko(tpl.id) {
                hay.push(' ');
                hay.push_str(ko.name);
                hay.push(' ');
                hay.push_str(ko.description);
            }
            for c in tpl.connectors {
                hay.push(' ');
                hay.push_str(super::View::source_label(*c));
            }
            for s in tpl.skills {
                hay.push(' ');
                hay.push_str(s);
                if let Some((title, _)) = bot_skill_ko(s) {
                    hay.push(' ');
                    hay.push_str(title);
                }
            }
            let hay = hay.to_lowercase();
            q.split_whitespace().all(|term| hay.contains(term))
        })
        .collect()
}

/// Localized label for a [`BotTemplateCategory`].
#[must_use]
pub fn category_label(cat: BotTemplateCategory) -> &'static str {
    match cat {
        BotTemplateCategory::Coding => text::CAT_CODING.get(),
        BotTemplateCategory::Releases => text::CAT_RELEASES.get(),
        BotTemplateCategory::Support => text::CAT_SUPPORT.get(),
        BotTemplateCategory::Planning => text::CAT_PLANNING.get(),
        BotTemplateCategory::Ops => text::CAT_OPS.get(),
        BotTemplateCategory::Personal => text::CAT_PERSONAL.get(),
    }
}

pub struct Templates {
    focus: FocusHandle,
    search: Entity<InputState>,
    query: String,
    category: Option<BotTemplateCategory>,
    selected: Option<&'static str>,
    /// Narrow windows show one template's details in place of the list.
    detail_only: bool,
    list_scroll: ScrollHandle,
    _subs: Vec<Subscription>,
}

impl EventEmitter<Event> for Templates {}

impl std::fmt::Debug for Templates {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Templates")
            .field("query", &self.query)
            .field("category", &self.category)
            .field("selected", &self.selected)
            .finish_non_exhaustive()
    }
}

fn category_icon(cat: Option<BotTemplateCategory>, color: gpui::Hsla) -> gpui::AnyElement {
    match cat {
        None => ui::icons::svg_icon(ui::icons::LAYOUT_TEMPLATE_SVG, ui::ICON_XS, color)
            .into_any_element(),
        Some(BotTemplateCategory::Coding) => {
            ui::icons::svg_icon(ui::icons::SQUARE_TERMINAL_SVG, ui::ICON_XS, color)
                .into_any_element()
        }
        Some(BotTemplateCategory::Releases) => {
            ui::icons::svg_icon(ui::icons::SPARKLES_SVG, ui::ICON_XS, color).into_any_element()
        }
        Some(BotTemplateCategory::Support) => Icon::new(gpui_kit_assets::IconName::MessagesSquare)
            .with_size(ui::ICON_XS)
            .text_color(color)
            .into_any_element(),
        Some(BotTemplateCategory::Planning) => Icon::new(IconName::FileText)
            .with_size(ui::ICON_XS)
            .text_color(color)
            .into_any_element(),
        Some(BotTemplateCategory::Ops) => {
            ui::icons::svg_icon(ui::icons::WRENCH_SVG, ui::ICON_XS, color).into_any_element()
        }
        Some(BotTemplateCategory::Personal) => Icon::new(IconName::User)
            .with_size(ui::ICON_XS)
            .text_color(color)
            .into_any_element(),
    }
}

impl Templates {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder(text::TPL_SEARCH.get()));
        let subs = vec![cx.subscribe_in(&search, window, Self::on_search)];
        search.update(cx, |s, cx| s.focus(window, cx));
        Self {
            focus: cx.focus_handle(),
            search,
            query: String::new(),
            category: None,
            selected: BUILTIN_BOT_TEMPLATES.first().map(|t| t.id),
            detail_only: false,
            list_scroll: ScrollHandle::new(),
            _subs: subs,
        }
    }

    #[must_use]
    pub fn focus_handle(&self) -> FocusHandle {
        self.focus.clone()
    }

    fn on_search(
        &mut self,
        input: &Entity<InputState>,
        ev: &InputEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match ev {
            InputEvent::Change => {
                self.query = input.read(cx).value().to_string();
                self.keep_selection_visible();
                cx.notify();
            }
            InputEvent::PressEnter { .. }
                if !crate::jump::entity_input_is_composing(input, window, cx) =>
            {
                self.use_selected(cx);
            }
            _ => {}
        }
    }

    fn visible(&self) -> Vec<&'static BotTemplate> {
        filter_templates(self.category, &self.query)
    }

    /// Keeps a visible row selected after the filter changes.
    fn keep_selection_visible(&mut self) {
        let visible = self.visible();
        if !visible.iter().any(|t| Some(t.id) == self.selected) {
            self.selected = visible.first().map(|t| t.id);
        }
    }

    pub fn set_category(&mut self, category: Option<BotTemplateCategory>, cx: &mut Context<Self>) {
        self.category = category;
        self.keep_selection_visible();
        cx.notify();
    }

    fn select(&mut self, id: &'static str, cx: &mut Context<Self>) {
        self.selected = Some(id);
        self.detail_only = true;
        cx.notify();
    }

    fn step(&mut self, delta: isize, cx: &mut Context<Self>) {
        let visible = self.visible();
        if visible.is_empty() {
            return;
        }
        let at = visible
            .iter()
            .position(|t| Some(t.id) == self.selected)
            .map_or(0, |i| {
                (i.cast_signed() + delta)
                    .clamp(0, visible.len().cast_signed() - 1)
                    .cast_unsigned()
            });
        self.selected = Some(visible[at].id);
        // The list also holds one header per shown category.
        self.list_scroll
            .scroll_to_item(list_child_index(&visible, at));
        cx.notify();
    }

    fn selected_template(&self) -> Option<&'static BotTemplate> {
        let id = self.selected?;
        BUILTIN_BOT_TEMPLATES.iter().find(|t| t.id == id)
    }

    pub fn use_selected(&mut self, cx: &mut Context<Self>) {
        if let Some(t) = self.selected_template()
            && self.visible().iter().any(|v| v.id == t.id)
        {
            cx.emit(Event::Use(t));
        }
    }

    fn blank(&mut self, cx: &mut Context<Self>) {
        let q = self.query.trim();
        cx.emit(Event::Blank((!q.is_empty()).then(|| q.to_owned())));
    }

    fn on_key(&mut self, e: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if crate::jump::entity_input_is_composing(&self.search, window, cx) {
            return;
        }
        match e.keystroke.key.as_str() {
            "escape" => {
                cx.stop_propagation();
                // Orca: Esc clears the search first, then closes.
                if self.query.is_empty() {
                    cx.emit(Event::Close);
                } else {
                    self.search.update(cx, |s, cx| s.set_value("", window, cx));
                    self.query.clear();
                    self.keep_selection_visible();
                    cx.notify();
                }
            }
            "up" => {
                cx.stop_propagation();
                self.step(-1, cx);
            }
            "down" => {
                cx.stop_propagation();
                self.step(1, cx);
            }
            _ => {}
        }
    }

    fn render_chips(&self, p: &ui::Palette, cx: &mut Context<Self>) -> gpui::Div {
        let chip = |id: SharedString,
                    cat: Option<BotTemplateCategory>,
                    label: &'static str,
                    active: bool| {
            let fg = if active {
                p.accent_foreground
            } else {
                p.muted_foreground
            };
            div()
                .id(id.clone())
                .debug_selector(move || id.to_string())
                .role(gpui::Role::Button)
                .tab_index(0)
                .focus_visible(|d| d.border_color(p.ring))
                .flex_none()
                .h(px(24.))
                .px_2()
                .rounded(ui::RADIUS_SM)
                .border_1()
                .flex()
                .items_center()
                .gap_1p5()
                .text_size(ui::TEXT_SM)
                .cursor_pointer()
                .when(active, |d| {
                    d.bg(p.accent)
                        .border_color(p.accent)
                        .text_color(p.accent_foreground)
                        .font_weight(FontWeight::MEDIUM)
                })
                .when(!active, |d| {
                    d.border_color(p.border)
                        .text_color(p.muted_foreground)
                        .hover(|d| d.bg(p.accent.opacity(0.5)))
                })
                .child(category_icon(cat, fg))
                .child(label)
        };
        let mut row = div().flex().flex_wrap().gap_1p5().child(
            chip(
                "bot-tpl-cat-all".into(),
                None,
                text::CAT_ALL.get(),
                self.category.is_none(),
            )
            .on_click(cx.listener(|this, _, _, cx| this.set_category(None, cx))),
        );
        for cat in BotTemplateCategory::ALL {
            let cat = *cat;
            row = row.child(
                chip(
                    format!("bot-tpl-cat-{cat:?}").to_lowercase().into(),
                    Some(cat),
                    category_label(cat),
                    self.category == Some(cat),
                )
                .on_click(cx.listener(move |this, _, _, cx| this.set_category(Some(cat), cx))),
            );
        }
        row
    }

    fn render_row(
        &self,
        tpl: &'static BotTemplate,
        p: &ui::Palette,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let current = self.selected == Some(tpl.id);
        let id = SharedString::from(format!("bot-tpl-{}", tpl.id));
        let use_id = SharedString::from(format!("bot-tpl-use-{}", tpl.id));
        let name = localized_template_name(tpl);
        let desc = localized_template_description(tpl);
        div()
            .id(id.clone())
            .debug_selector(move || id.to_string())
            .role(gpui::Role::ListItem)
            .aria_label(name)
            .group("bot-tpl-row")
            .w_full()
            .px_2()
            .py_2p5()
            .rounded(ui::RADIUS_SM)
            .flex()
            .items_center()
            .gap_3()
            .cursor_pointer()
            .when(current, |d| d.bg(p.accent))
            .when(!current, |d| d.hover(|d| d.bg(p.accent.opacity(0.5))))
            .on_click(cx.listener(move |this, e: &gpui::ClickEvent, _, cx| {
                if e.click_count() >= 2 {
                    cx.emit(Event::Use(tpl));
                } else {
                    this.select(tpl.id, cx);
                }
            }))
            .child(template_mascot(tpl, 32., ui::RADIUS_LG, p))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1p5()
                            .child(
                                div()
                                    .truncate()
                                    .text_size(ui::TEXT_LG)
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(p.foreground)
                                    .child(name),
                            )
                            .when(tpl.needs_setup.is_some(), |d| {
                                d.child(ui::badge(
                                    p,
                                    ui::BadgeTone::Muted,
                                    text::TPL_NEEDS_SETUP.get(),
                                ))
                            }),
                    )
                    .child(
                        div()
                            .truncate()
                            .text_size(ui::TEXT_SM)
                            .text_color(p.muted_foreground)
                            .child(desc),
                    ),
            )
            .child(
                div()
                    .flex_none()
                    .when(!current, |d| {
                        d.invisible()
                            .group_hover("bot-tpl-row", gpui::Styled::visible)
                    })
                    .child(
                        ui::secondary(use_id.clone(), text::TPL_USE.get())
                            .xsmall()
                            .accessibility_label(text::TPL_USE_NAMED.format(&[("name", name)]))
                            .debug_selector(move || use_id.to_string())
                            .on_click(cx.listener(move |_, _, _, cx| {
                                cx.stop_propagation();
                                cx.emit(Event::Use(tpl));
                            })),
                    ),
            )
    }

    fn render_list(&self, p: &ui::Palette, cx: &mut Context<Self>) -> gpui::Stateful<gpui::Div> {
        let visible = self.visible();
        let mut list = div()
            .id("bot-tpl-list")
            .debug_selector(|| "bot-tpl-list".into())
            .role(gpui::Role::List)
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .track_scroll(&self.list_scroll)
            .px_6()
            .pb_8()
            .flex()
            .flex_col();
        if visible.is_empty() {
            let q = self.query.trim().to_owned();
            return list.child(
                div()
                    .id("bot-tpl-empty")
                    .debug_selector(|| "bot-tpl-empty".into())
                    .pt_10()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap_2()
                    .text_center()
                    .child(
                        div()
                            .text_size(ui::TEXT_LG)
                            .font_weight(FontWeight::MEDIUM)
                            .child(text::TPL_NO_MATCH.format(&[("query", &q)])),
                    )
                    .child(
                        div()
                            .text_size(ui::TEXT_SM)
                            .text_color(p.muted_foreground)
                            .child(text::TPL_NO_MATCH_HINT.get()),
                    )
                    .child(
                        div()
                            .mt_2()
                            .flex()
                            .gap_2()
                            .child(
                                ui::secondary(
                                    "bot-tpl-blank-for",
                                    text::TPL_BLANK_FOR.format(&[("query", &q)]),
                                )
                                .on_click(cx.listener(|this, _, _, cx| this.blank(cx))),
                            )
                            .child(
                                ui::ghost("bot-tpl-clear", text::SB_CLEAR_SEARCH.get()).on_click(
                                    cx.listener(|this, _, window, cx| {
                                        this.search.update(cx, |s, cx| s.set_value("", window, cx));
                                        this.query.clear();
                                        this.keep_selection_visible();
                                        cx.notify();
                                    }),
                                ),
                            ),
                    ),
            );
        }
        for cat in BotTemplateCategory::ALL {
            let in_cat: Vec<_> = visible.iter().filter(|t| t.category == *cat).collect();
            if in_cat.is_empty() {
                continue;
            }
            list = list.child(
                div()
                    .py_2()
                    .bg(p.popover)
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .text_size(ui::TEXT_SM)
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(p.muted_foreground)
                    .child(category_icon(Some(*cat), p.muted_foreground))
                    .child(category_label(*cat)),
            );
            for tpl in in_cat {
                list = list.child(self.render_row(tpl, p, cx));
            }
        }
        list
    }

    fn render_detail(
        tpl: &'static BotTemplate,
        narrow: bool,
        p: &ui::Palette,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let name = localized_template_name(tpl);
        let desc = localized_template_description(tpl);
        let first_chat = localized_template_first_chat(tpl);
        let needs_setup = localized_template_needs_setup(tpl);
        let examples = localized_template_examples(tpl);
        let section = |title: &'static str| {
            div()
                .text_size(ui::TEXT_SM)
                .font_weight(FontWeight::MEDIUM)
                .text_color(p.foreground)
                .child(title)
        };
        let comes_with = comes_with_rows(tpl, p, |_| None);
        div()
            .id("bot-tpl-detail")
            .debug_selector(|| "bot-tpl-detail".into())
            .h_full()
            .min_h_0()
            .flex()
            .flex_col()
            .when(!narrow, |d| {
                d.w(gpui::relative(0.3))
                    .min_w(px(400.))
                    .border_l_1()
                    .border_color(p.border)
            })
            .when(narrow, gpui::Styled::flex_1)
            .child(
                div()
                    .flex_none()
                    .px_6()
                    .pt_6()
                    .pb_5()
                    .flex()
                    .flex_col()
                    .gap_5()
                    .when(narrow, |d| {
                        d.child(
                            ui::ghost("bot-tpl-back", text::CAT_ALL.get())
                                .icon(IconName::ArrowLeft)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.detail_only = false;
                                    cx.notify();
                                })),
                        )
                    })
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_3()
                            .child(template_mascot(tpl, 56., ui::RADIUS_XL, p))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .flex()
                                    .flex_col()
                                    .gap_0p5()
                                    .child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .gap_1p5()
                                            .child(
                                                div()
                                                    .text_size(ui::TEXT_XL)
                                                    .font_weight(FontWeight::SEMIBOLD)
                                                    .child(name),
                                            )
                                            .when(tpl.needs_setup.is_some(), |d| {
                                                d.child(ui::badge(
                                                    p,
                                                    ui::BadgeTone::Muted,
                                                    text::TPL_NEEDS_SETUP.get(),
                                                ))
                                            }),
                                    )
                                    .child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .gap_1()
                                            .text_size(ui::TEXT_SM)
                                            .text_color(p.muted_foreground)
                                            .child(
                                                Icon::new(IconName::CircleCheck)
                                                    .with_size(ui::ICON_XS)
                                                    .text_color(p.muted_foreground),
                                            )
                                            .child(text::TPL_BY_KRILL.format(&[(
                                                "category",
                                                category_label(tpl.category),
                                            )])),
                                    ),
                            ),
                    )
                    .child(div().text_size(ui::TEXT_LG).child(desc))
                    .child(
                        ui::primary("bot-tpl-use-detail", text::TPL_USE_TEMPLATE.get())
                            .debug_selector(|| "bot-tpl-use-detail".into())
                            .on_click(cx.listener(move |_, _, _, cx| cx.emit(Event::Use(tpl)))),
                    ),
            )
            .child(
                div()
                    .id("bot-tpl-detail-body")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .px_6()
                    .pb_6()
                    .flex()
                    .flex_col()
                    .gap_5()
                    .child(
                        div()
                            .p_3()
                            .rounded(ui::RADIUS_MD)
                            .border_1()
                            .border_color(p.border)
                            .bg(p.muted.opacity(0.4))
                            .flex()
                            .flex_col()
                            .gap_2()
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_1p5()
                                    .text_size(ui::TEXT_SM)
                                    .text_color(p.muted_foreground)
                                    .child(template_mascot(tpl, 16., ui::RADIUS_XS, p))
                                    .child(text::TPL_IN_ITS_CHAT.format(&[("name", name)])),
                            )
                            .children(
                                examples
                                    .into_iter()
                                    .filter(|l| !l.is_empty())
                                    .map(|line| div().text_size(ui::TEXT_LG).child(line)),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(section(text::TPL_FIRST.get()))
                            .child(
                                div()
                                    .text_size(ui::TEXT_LG)
                                    .text_color(p.muted_foreground)
                                    .child(first_chat),
                            ),
                    )
                    .children(needs_setup.map(|note| {
                        div()
                            .p_3()
                            .rounded(ui::RADIUS_MD)
                            .border_1()
                            .border_color(p.border)
                            .bg(p.muted.opacity(0.4))
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(section(text::TPL_NEEDS_SETUP.get()))
                            .child(
                                div()
                                    .text_size(ui::TEXT_SM)
                                    .text_color(p.muted_foreground)
                                    .child(note),
                            )
                    }))
                    .when(!comes_with.is_empty(), |d| {
                        d.child(
                            div()
                                .flex()
                                .flex_col()
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap_1()
                                        .child(section(text::NB_COMES_WITH.get()))
                                        .child(ui::button_with_tooltip(
                                            Button::new("bot-tpl-comes-with-info")
                                                .ghost()
                                                .xsmall()
                                                .icon(
                                                    Icon::new(IconName::Info)
                                                        .with_size(ui::ICON_XS)
                                                        .text_color(p.muted_foreground),
                                                ),
                                            "bot-tpl-comes-with-info",
                                            text::TPL_COMES_WITH_INFO.get(),
                                        )),
                                )
                                .children(comes_with),
                        )
                    }),
            )
    }
}

/// Position of `visible[at]` among the list's children (category headers included).
fn list_child_index(visible: &[&BotTemplate], at: usize) -> usize {
    let mut child = 0;
    for cat in BotTemplateCategory::ALL {
        let in_cat: Vec<usize> = (0..visible.len())
            .filter(|i| visible[*i].category == *cat)
            .collect();
        if in_cat.is_empty() {
            continue;
        }
        child += 1;
        if let Some(pos) = in_cat.iter().position(|i| *i == at) {
            return child + pos;
        }
        child += in_cat.len();
    }
    child
}

/// The template's mascot on the default background.
fn template_mascot(
    tpl: &BotTemplate,
    size: f32,
    radius: gpui::Pixels,
    p: &ui::Palette,
) -> gpui::AnyElement {
    let face = avatar::Avatar::from_fields(Some(tpl.mascot), None, None, tpl.name);
    avatar::render(&face, None, tpl.name, px(size), radius, p)
}

/// Orca's "Comes with" rows: skills (with `skill_trailing`, e.g. New bot's checkbox),
/// schedules (off), events (first chat) and connectors (not connected).
pub(super) fn comes_with_rows(
    tpl: &'static BotTemplate,
    p: &ui::Palette,
    mut skill_trailing: impl FnMut(&'static str) -> Option<gpui::AnyElement>,
) -> Vec<gpui::Div> {
    let svg = |icon: &'static [u8]| {
        ui::icons::svg_icon(icon, ui::ICON_LG, p.muted_foreground).into_any_element()
    };
    let status = |s: &'static str, emphasize: bool| {
        div()
            .text_size(ui::TEXT_SM)
            .text_color(if emphasize {
                p.foreground
            } else {
                p.muted_foreground
            })
            .child(s)
            .into_any_element()
    };
    let row = |icon: gpui::AnyElement,
               title: SharedString,
               detail: Option<SharedString>,
               trailing: Option<gpui::AnyElement>| {
        div()
            .w_full()
            .py_2p5()
            .border_b_1()
            .border_color(p.border.opacity(0.6))
            .flex()
            .items_start()
            .gap_3()
            .child(div().pt_0p5().flex_none().child(icon))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .child(div().text_size(ui::TEXT_SM).child(title))
                    .children(detail.map(|d| {
                        div()
                            .text_size(ui::TEXT_SM)
                            .text_color(p.muted_foreground)
                            .child(d)
                    })),
            )
            .children(trailing.map(|t| div().flex_none().child(t)))
    };
    let mut rows = Vec::new();
    for skill in tpl.skills {
        let book = gpui_component::Icon::new(IconName::BookOpen)
            .with_size(ui::ICON_LG)
            .text_color(p.muted_foreground)
            .into_any_element();
        let detail = find_bot_skill(skill)
            .map(|s| localized_skill_description(s.name, s.description).into());
        rows.push(row(
            book,
            localized_skill_title(skill).into(),
            detail,
            skill_trailing(skill),
        ));
    }
    for s in tpl.schedules {
        let (when, what) = localized_schedule(tpl.id, s);
        rows.push(row(
            svg(ui::icons::CLOCK_SVG),
            when.into(),
            Some(what.into()),
            Some(status(text::NB_OFF.get(), false)),
        ));
    }
    for e in tpl.events {
        rows.push(row(
            svg(ui::icons::ZAP_SVG),
            localized_event_name(tpl.id, e).into(),
            None,
            Some(status(text::NB_FIRST_CHAT.get(), false)),
        ));
    }
    for c in tpl.connectors {
        rows.push(row(
            svg(ui::icons::PLUG_SVG),
            super::View::source_label(*c).into(),
            None,
            Some(status(text::NB_NOT_CONNECTED.get(), true)),
        ));
    }
    rows
}

impl Render for Templates {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = ui::palette(cx);
        let viewport = window.viewport_size();
        let width = px(WIDTH).min(viewport.width - ui::SPACE_4 * 2.);
        let narrow = width < px(SPLIT_MIN_WIDTH);
        let selected = self
            .selected_template()
            .filter(|t| self.visible().iter().any(|v| v.id == t.id));
        let show_detail_only = narrow && self.detail_only && selected.is_some();

        let left = div()
            .flex_1()
            .min_w_0()
            .h_full()
            .min_h_0()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex_none()
                    .px_6()
                    .pt_4()
                    .pb_2()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        Input::new(&self.search).small().prefix(
                            gpui_component::Icon::new(IconName::Search)
                                .with_size(ui::ICON_MD)
                                .text_color(p.muted_foreground),
                        ),
                    )
                    .child(self.render_chips(p, cx)),
            )
            .child(self.render_list(p, cx));

        let body = div().flex_1().min_h_0().flex();
        let body = if show_detail_only {
            body.children(selected.map(|t| Self::render_detail(t, true, p, cx)))
        } else if narrow {
            body.child(left)
        } else {
            body.child(left)
                .children(selected.map(|t| Self::render_detail(t, false, p, cx)))
        };

        let card = div()
            .id("bot-templates")
            .debug_selector(|| "bot-templates".into())
            .role(gpui::Role::Dialog)
            .aria_label(text::TPL_TITLE.get())
            .occlude()
            .w(width)
            .h(px(HEIGHT).min(viewport.height - ui::SPACE_4 * 2.))
            .rounded(ui::RADIUS_LG)
            .bg(p.popover)
            .text_color(p.popover_foreground)
            .font_family(ui::FONT_UI)
            .text_size(ui::TEXT_MD)
            .border_1()
            .border_color(p.border)
            .shadow_lg()
            .overflow_hidden()
            .flex()
            .flex_col()
            .capture_key_down(
                cx.listener(|this, e: &KeyDownEvent, window, cx| this.on_key(e, window, cx)),
            )
            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                div()
                    .flex_none()
                    .py_3p5()
                    .pl_6()
                    .pr_4()
                    .border_b_1()
                    .border_color(p.border)
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .text_size(ui::TEXT_LG)
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(text::TPL_TITLE.get()),
                    )
                    .child(
                        ui::secondary("bot-tpl-blank", text::TPL_BLANK.get())
                            .icon(IconName::Plus)
                            .debug_selector(|| "bot-tpl-blank".into())
                            .on_click(cx.listener(|this, _, _, cx| this.blank(cx))),
                    )
                    .child(div().h(px(20.)).w(px(1.)).bg(p.border.opacity(0.5)))
                    .child(ui::button_with_tooltip(
                        Button::new("bot-tpl-close")
                            .ghost()
                            .small()
                            .icon(IconName::Close)
                            .debug_selector(|| "bot-tpl-close".into())
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(Event::Close))),
                        "bot-tpl-close",
                        text::TPL_CLOSE.get(),
                    )),
            )
            .child(body);
        ui::modal_backdrop(
            viewport,
            div()
                .id("bot-templates-backdrop")
                .debug_selector(|| "bot-templates-backdrop".into())
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .on_mouse_down(
                    gpui::MouseButton::Left,
                    cx.listener(|_, _, _, cx| cx.emit(Event::Close)),
                )
                .child(ui::modal_focus_scope(card, &self.focus)),
        )
    }
}

impl gpui::Focusable for Templates {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_matches_every_word_across_name_connectors_and_skills() {
        assert_eq!(
            filter_templates(None, "").len(),
            BUILTIN_BOT_TEMPLATES.len()
        );
        let coding = filter_templates(Some(BotTemplateCategory::Coding), "");
        assert!(
            coding
                .iter()
                .all(|t| t.category == BotTemplateCategory::Coding)
        );
        assert!(
            filter_templates(None, "pull request")
                .iter()
                .any(|t| t.id == "pr-review")
        );
        // Connector names count (Orca searches them too).
        let slack = filter_templates(None, "slack");
        assert!(!slack.is_empty());
        assert!(slack.iter().all(|t| {
            t.connectors
                .contains(&krill_proto::bots::ExternalSource::Slack)
                || t.name.to_lowercase().contains("slack")
                || t.description.to_lowercase().contains("slack")
                || t.skills.iter().any(|s| s.contains("slack"))
        }));
        assert!(filter_templates(None, "zzzz-no-such-template").is_empty());
    }

    #[test]
    fn all_builtin_templates_have_korean_translations_and_search() {
        for tpl in BUILTIN_BOT_TEMPLATES {
            assert!(
                bot_template_ko(tpl.id).is_some(),
                "missing Korean translation for template {}",
                tpl.id
            );
            for sched in tpl.schedules {
                assert!(
                    bot_template_schedule_ko(tpl.id, sched.id).is_some(),
                    "missing Korean schedule translation for {}:{}",
                    tpl.id,
                    sched.id
                );
            }
            for ev in tpl.events {
                assert!(
                    bot_template_event_ko(tpl.id, ev.id).is_some(),
                    "missing Korean event translation for {}:{}",
                    tpl.id,
                    ev.id
                );
            }
            for skill in tpl.skills {
                assert!(
                    bot_skill_ko(skill).is_some(),
                    "missing Korean skill translation for {skill}"
                );
            }
        }
        assert!(
            filter_templates(None, "이메일")
                .iter()
                .any(|t| t.id == "email-digest")
        );
        assert!(
            filter_templates(None, "버그 조사")
                .iter()
                .any(|t| t.id == "bug-investigation")
        );
    }
}
