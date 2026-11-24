// Adapted from Orca (Copyright (c) 2026 Lovecast Inc., MIT License).
//! The Bots section's header (Orca `SidebarBotsSection` + `SidebarSectionHeader`): fold
//! chevron and title, collapse-all on hover, folded faces, then search, "Bot options" and "+".
//! Also the search results (`BotSidebarSearch`, `BotSearchResultRow`).

use gpui::{Context, FontWeight, SharedString, Window, div, prelude::*, px};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::Input;
use gpui_component::menu::{DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_component::{IconName, Sizable as _};
use krill_app::bots::SidebarBot;
use krill_app::bots::sidebar_view::{self as view, BotSidebarSort, BotSidebarView};
use krill_app::i18n::catalog::bots as text;
use krill_app::sidebar::options as menu_parts;
use krill_app::ui;

use super::{BotsScreen, Shell};

impl Shell {
    /// The Bots options, read from settings the first time.
    pub(super) fn bot_view(&self, cx: &gpui::App) -> BotSidebarView {
        self.bot_ui
            .view
            .unwrap_or_else(|| krill_app::settings::bot_sidebar_view(cx))
    }

    pub(super) fn set_bot_view(&mut self, view: BotSidebarView, cx: &mut Context<Self>) {
        self.bot_ui.view = Some(view);
        krill_app::settings::set_bot_sidebar_view(view, cx);
        cx.notify();
    }

    /// Orca `useBotCollapseAll`: offered while some listed bot has something to fold.
    fn bots_all_folded(&self) -> bool {
        self.sidebar_bots
            .iter()
            .all(|b| self.bot_ui.folded.contains(&(b.host.clone(), b.id.clone())))
    }

    pub(super) fn toggle_fold_all_bots(&mut self, cx: &mut Context<Self>) {
        if self.bots_all_folded() {
            self.bot_ui.folded.clear();
        } else {
            self.bot_ui.folded = self
                .sidebar_bots
                .iter()
                .map(|b| (b.host.clone(), b.id.clone()))
                .collect();
        }
        cx.notify();
    }

    pub(super) fn render_bots_header(
        &self,
        p: &'static ui::Palette,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let collapsed = self.bot_ui.collapsed;
        let searching = self.bot_ui.search.is_some();
        let has_bots = !self.sidebar_bots.is_empty();
        let has_entries = has_bots || !self.sidebar_groups.is_empty();
        let all_folded = self.bots_all_folded();
        let fold_all_label = if all_folded {
            text::SB_EXPAND_ALL.get()
        } else {
            text::SB_COLLAPSE_ALL.get()
        };

        let mut title = div()
            .id("bots-section-title")
            .debug_selector(|| "bots-section-title".into())
            .role(gpui::Role::Button)
            .aria_label(if collapsed {
                text::SB_EXPAND.get()
            } else {
                text::SB_COLLAPSE.get()
            })
            .aria_expanded(!collapsed)
            .group("bots-section-title")
            .cursor_pointer()
            .flex_none()
            .flex()
            .items_center()
            .gap_1()
            .on_click(cx.listener(|this, _, _, cx| {
                this.bot_ui.collapsed = !this.bot_ui.collapsed;
                cx.notify();
            }))
            .child(
                div()
                    .ml_0p5()
                    .size_5()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(ui::RADIUS_SM)
                    .text_color(p.muted_foreground)
                    .group_hover("bots-section-title", |d| {
                        d.bg(p.accent.opacity(0.7)).text_color(p.foreground)
                    })
                    .child(
                        gpui_component::Icon::new(if collapsed {
                            IconName::ChevronRight
                        } else {
                            IconName::ChevronDown
                        })
                        .size(px(14.)),
                    ),
            )
            .child(
                div()
                    .pr_0p5()
                    .text_size(ui::TEXT_XS)
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(p.muted_foreground.opacity(0.8))
                    .child(text::SB_SECTION_TITLE.get()),
            );
        // Orca `SectionCollapseAllButton`: right after the title, shown while the header is
        // hovered.
        if !collapsed && has_bots && !searching {
            title = title.child(
                div()
                    .id("bots-collapse-all-wrap")
                    .invisible()
                    .group_hover("bots-section-header", gpui::Styled::visible)
                    .child(
                        Button::new("bots-collapse-all")
                            .ghost()
                            .xsmall()
                            .icon(
                                gpui_component::Icon::default()
                                    .data(if all_folded {
                                        ui::icons::CHEVRONS_UP_DOWN_SVG
                                    } else {
                                        ui::icons::CHEVRONS_DOWN_UP_SVG
                                    })
                                    .size(px(14.)),
                            )
                            .accessibility_label(fold_all_label)
                            .tooltip(fold_all_label)
                            .debug_selector(|| "bots-collapse-all".into())
                            .on_click(cx.listener(|this, _, _, cx| {
                                cx.stop_propagation();
                                this.toggle_fold_all_bots(cx);
                            })),
                    ),
            );
        }

        let header = div()
            .id("bots-section-header")
            .debug_selector(|| "bots-section-header".into())
            .group("bots-section-header")
            // Orca SidebarSectionHeader: `mt-2 h-8 px-2 gap-1.5`, a fold chevron before the title.
            .mt_1()
            .h(ui::SIDEBAR_HEADER_HEIGHT)
            .px_2()
            .flex()
            .items_center()
            .gap_1p5()
            .child(title);
        if collapsed {
            return header.child(self.render_collapsed_faces(p, cx));
        }
        header.child(div().flex_1()).child(
            div()
                .flex_none()
                .flex()
                .items_center()
                .gap_1()
                .when(has_entries, |d| {
                    d.child(
                        Button::new("bots-section-search")
                            .ghost()
                            .xsmall()
                            .toggled(searching)
                            .icon(gpui_component::Icon::new(IconName::Search).size(px(14.)))
                            .accessibility_label(text::SB_SEARCH.get())
                            .when(!searching, |b| b.tooltip(text::SB_SEARCH.get()))
                            .debug_selector(|| "bots-section-search".into())
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.toggle_bot_search(window, cx);
                            })),
                    )
                    .child(self.render_bot_options_button(p, cx))
                })
                .child(self.render_bots_new_menu(cx)),
        )
    }

    fn render_bots_new_menu(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let groups_ok = self.default_group_host().is_some();
        let this = cx.weak_entity();
        Button::new("bots-section-new")
            .ghost()
            .xsmall()
            .icon(gpui_component::Icon::new(IconName::Plus).size(px(14.)))
            .accessibility_label(text::SB_NEW_MENU.get())
            .tooltip(text::SB_NEW_MENU.get())
            .debug_selector(|| "bots-section-new".into())
            .dropdown_menu(move |menu, _, _| {
                let act = |f: fn(&mut Shell, &mut Window, &mut Context<Shell>)| {
                    let this = this.clone();
                    move |_: &gpui::ClickEvent, window: &mut Window, cx: &mut gpui::App| {
                        this.update(cx, |s, cx| f(s, window, cx)).ok();
                    }
                };
                menu.item(
                    PopupMenuItem::new(text::SB_NEW_BOT.get())
                        .icon(gpui_component::Icon::default().data(ui::icons::BOT_SVG))
                        .on_click(act(|s, _, cx| s.request_new_bot(None, None, cx))),
                )
                .item(
                    PopupMenuItem::new(text::SB_BROWSE_TEMPLATES.get())
                        .icon(gpui_component::Icon::default().data(ui::icons::LAYOUT_TEMPLATE_SVG))
                        .on_click(act(|s, _, cx| s.request_bot_templates(cx))),
                )
                .item(
                    PopupMenuItem::new(text::SB_IMPORT.get())
                        .icon(gpui_component::Icon::default().data(ui::icons::DOWNLOAD_SVG))
                        .on_click(act(|s, window, cx| {
                            s.open_bots_screen(BotsScreen::Import, Some(window), cx);
                        })),
                )
                .separator()
                .item(
                    PopupMenuItem::new(text::SB_NEW_GROUP.get())
                        .icon(gpui_component::Icon::default().data(ui::icons::USERS_SVG))
                        .disabled(!groups_ok)
                        .on_click(act(|s, window, cx| {
                            s.open_new_group_dialog(window, cx);
                        })),
                )
            })
    }

    /// Orca `BotSidebarOptionsMenu`: Sort by, then the two filters, with a count badge.
    fn render_bot_options_button(
        &self,
        p: &'static ui::Palette,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let active = self.bot_view(cx).active_filters();
        let label: SharedString = if active > 0 {
            text::SBO_TITLE_ACTIVE
                .format(&[("n", &active.to_string())])
                .into()
        } else {
            text::SBO_TITLE.get().into()
        };
        let this = cx.weak_entity();
        div()
            .id("bots-options-wrap")
            .debug_selector(|| "bots-options".into())
            .flex_none()
            .flex()
            .items_center()
            .gap_0p5()
            .child(
                Button::new("bots-options")
                    .ghost()
                    .xsmall()
                    .toggled(active > 0)
                    .icon(
                        gpui_component::Icon::default()
                            .data(ui::icons::SLIDERS_HORIZONTAL_SVG)
                            .size(px(14.)),
                    )
                    .accessibility_label(label.clone())
                    .tooltip(text::SBO_TITLE.get())
                    .dropdown_menu(move |menu, window, cx| {
                        bot_options_menu(menu, &this, window, cx)
                    }),
            )
            .when(active > 0, |d| {
                d.child(
                    div()
                        .id("bots-options-badge")
                        .debug_selector(|| "bots-options-badge".into())
                        .min_w(px(14.))
                        .h(px(14.))
                        .px_1()
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded_full()
                        .bg(p.primary)
                        .font_family(ui::FONT_MONO)
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_size(ui::TEXT_2XS)
                        .text_color(p.primary_foreground)
                        .child(active.to_string()),
                )
            })
    }

    /// Orca `BotCollapsedPreview` + `bot-preview-indicators` (`#47622`, `#47884`):
    /// virtualized horizontal face track with left/right overflow edge controls (`BotPreviewEdge`),
    /// clipped edge-peek avatars suppressing pips, active bot `ring-sidebar-foreground` ring,
    /// and hover card details. Kept flush next to the `Bots` label (`justify_start`).
    fn render_collapsed_faces(
        &self,
        p: &'static ui::Palette,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let focused = self.focused_session();
        let mut bots: Vec<(
            usize,
            &SidebarBot,
            super::rows::BotFacts,
            Option<view::PreviewSignal>,
        )> = self
            .sidebar_bots
            .iter()
            .enumerate()
            .map(|(idx, b)| {
                let bot_focused = focused
                    .as_ref()
                    .and_then(|(h, id)| (*h == b.host).then_some(*id));
                let facts = self.bot_facts(b, bot_focused, cx);
                let needs_you = facts.needs_you || b.attention > 0;
                let working = !b.paused
                    && (facts.status == super::rows::RowStatus::Working || b.working() > 0);
                let signal = view::PreviewSignal::from_flags(needs_you, facts.unread, working);
                (idx, b, facts, signal)
            })
            .collect();
        bots.sort_by_key(|(idx, _, _, signal)| (view::PreviewSignal::rank(*signal), *idx));

        let available_px = (self.layout.sidebar_width - 88.).max(64.);
        let win =
            view::compute_collapsed_track(bots.len(), self.bot_ui.preview_offset, available_px);

        let edge_signal = |slice: &[(
            usize,
            &SidebarBot,
            super::rows::BotFacts,
            Option<view::PreviewSignal>,
        )]|
         -> (Option<view::PreviewSignal>, usize) {
            let best = slice.iter().filter_map(|(_, _, _, sig)| *sig).min();
            let attention = slice
                .iter()
                .filter(|(_, _, _, sig)| *sig == Some(view::PreviewSignal::Attention))
                .count();
            (best, attention)
        };

        let mut track = div()
            .h(px(view::PREVIEW_TRACK_HEIGHT))
            .min_w_0()
            .flex()
            .items_center()
            .overflow_hidden();

        if win.before > 0 {
            let (sig, attn) = edge_signal(&bots[..win.before]);
            let next_offset = win.offset.saturating_sub(win.page_step);
            track = track.child(render_preview_edge(
                -1,
                win.before,
                attn,
                sig,
                p,
                cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.bot_ui.preview_offset = next_offset;
                    cx.notify();
                }),
                cx,
            ));
        }

        let mut strip = div()
            .h_full()
            .min_w_0()
            .flex()
            .items_center()
            .gap(px(view::PREVIEW_FACE_GAP))
            .px(px(view::PREVIEW_TRACK_INSET))
            .overflow_hidden();

        for (idx, (_, bot, facts, signal)) in bots
            .iter()
            .enumerate()
            .take(win.end_visible)
            .skip(win.first_visible)
        {
            let clipped = idx < win.first_full || idx >= win.end_full;
            let host = bot.host.clone();
            let id = bot.id.clone();
            let sel = format!("bots-collapsed-{}", bot.id.as_str());
            let elem_id = if bot.host.is_local() {
                sel.clone()
            } else {
                format!("bots-collapsed-{}-{}", bot.host.as_key(), bot.id.as_str())
            };
            let pip: Option<gpui::AnyElement> = if clipped {
                None
            } else {
                render_preview_pip(bot.id.as_str(), *signal, &self.bot_ui.spinners, p, cx)
            };
            let hover = self.bot_hover_card(bot, facts, cx.weak_entity(), cx);
            let is_current = facts.active;
            let face = div()
                .id(SharedString::from(elem_id))
                .debug_selector(move || sel.clone())
                .role(gpui::Role::Button)
                .aria_label(bot.name.clone())
                .relative()
                .flex_none()
                .rounded_full()
                .border_2()
                .border_color(if is_current {
                    p.sidebar_foreground
                } else {
                    p.sidebar
                })
                .when(!is_current, |d| d.hover(|d| d.border_color(p.ring)))
                .cursor_pointer()
                .hoverable_tooltip(hover)
                .on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.bot_ui.collapsed = false;
                    this.open_bot_home_on(&host, id.clone(), None, cx);
                }))
                .child(self.bot_avatar(
                    bot,
                    view::PREVIEW_FACE_SIZE,
                    px(view::PREVIEW_FACE_SIZE / 2.),
                    p,
                ))
                .children(pip);

            if clipped {
                let clip_sel = format!("bots-collapsed-clipped-{}", bot.id.as_str());
                strip = strip.child(
                    div()
                        .debug_selector(move || clip_sel.clone())
                        .flex_none()
                        .w(px(10.))
                        .overflow_hidden()
                        .opacity(0.55)
                        .child(face),
                );
            } else {
                strip = strip.child(face);
            }
        }
        track = track.child(strip);

        if win.after > 0 {
            let (sig, attn) = edge_signal(&bots[bots.len() - win.after..]);
            let next_offset = (win.offset + win.page_step).min(win.max_offset);
            track = track.child(render_preview_edge(
                1,
                win.after,
                attn,
                sig,
                p,
                cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.bot_ui.preview_offset = next_offset;
                    cx.notify();
                }),
                cx,
            ));
        }

        div()
            .id("bots-collapsed-preview")
            .debug_selector(|| "bots-collapsed-preview".into())
            .min_w_0()
            .flex_1()
            .flex()
            .items_center()
            .justify_start()
            .overflow_hidden()
            .pl_0p5()
            .pr_1()
            .child(track)
    }

    /// Orca `BotSidebarSearchPanel`: the field under the header, then (with a query) results in
    /// place of the list.
    pub(super) fn render_bot_search(
        &self,
        order: &[usize],
        facts_all: &[super::rows::BotFacts],
        p: &'static ui::Palette,
        cx: &mut Context<Self>,
    ) -> Option<gpui::Div> {
        let (input, _) = self.bot_ui.search.as_ref()?;
        let field = div().px_2().pt_1().pb_1p5().child(
            Input::new(input).small().cleanable(true).prefix(
                gpui_component::Icon::new(IconName::Search)
                    .size(px(14.))
                    .text_color(p.muted_foreground),
            ),
        );
        let query = self.bot_ui.query.trim();
        if query.is_empty() {
            return Some(field);
        }
        let shown: Vec<(&SidebarBot, &super::rows::BotFacts)> = order
            .iter()
            .map(|&i| (&self.sidebar_bots[i], &facts_all[i]))
            .collect();
        let candidates: Vec<view::SearchCandidate<'_>> = shown
            .iter()
            .map(|(b, f)| view::SearchCandidate {
                name: b.name.as_str(),
                purpose: b.description.as_str(),
                chats: f
                    .chats
                    .iter()
                    .map(|c| (c.title.as_ref(), c.status.as_deref()))
                    .collect(),
            })
            .collect();
        let hidden: Vec<&str> = self
            .sidebar_bots
            .iter()
            .filter(|b| b.paused && !shown.iter().any(|(s, _)| s.host == b.host && s.id == b.id))
            .map(|b| b.name.as_str())
            .collect();
        let (matches, hidden_paused) = view::search_with_chats(&candidates, &hidden, query);
        let mut list = div()
            .id("bots-search-results")
            .debug_selector(|| "bots-search-results".into())
            .role(gpui::Role::List)
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .pb_1()
            .pl_1()
            .pr_1()
            .flex()
            .flex_col();
        for m in &matches {
            let (bot, bot_facts) = shown[m.index];
            list = list.child(self.render_bot_search_result(bot, bot_facts, m, p, cx));
        }
        let mut group_matches = 0usize;
        for summary in &self.sidebar_groups {
            let group_host = summary.host.clone();
            let group = &summary.group;
            let host_bots: Vec<SidebarBot> = self
                .sidebar_bots
                .iter()
                .filter(|b| b.host == group_host)
                .cloned()
                .collect();
            let name_ranges = view::match_text(&group.name, query);
            let member_match = name_ranges.is_none()
                && group.bot_ids.iter().any(|bid| {
                    host_bots
                        .iter()
                        .find(|b| &b.id == bid)
                        .is_some_and(|b| view::match_text(&b.name, query).is_some())
                });
            if name_ranges.is_some() || member_match {
                group_matches += 1;
                let ranges = name_ranges.unwrap_or_default();
                let gid = group.id.clone();
                let face_members = krill_app::bots::group_faces::resolve_members(
                    &group.bot_ids,
                    &host_bots,
                    &|id| {
                        self.bot_ui
                            .avatars
                            .get(&(group_host.clone(), id.clone()))
                            .and_then(|(_, img)| img.clone())
                    },
                );
                let faces = krill_app::bots::group_faces::render_group_faces(
                    &face_members,
                    super::rows::AVATAR,
                    p.worktree_sidebar,
                    p,
                );
                let sel = format!("bot-group-search-result-{gid}");
                let elem_id = if group_host.is_local() {
                    sel.clone()
                } else {
                    format!("bot-group-search-result-{}-{gid}", group_host.as_key())
                };
                let sel_dbg = sel.clone();
                let member_names: Vec<&str> =
                    face_members.iter().map(|m| m.bot.name.as_str()).collect();
                list = list.child(
                    div()
                        .id(SharedString::from(elem_id))
                        .debug_selector(move || sel_dbg.clone())
                        .role(gpui::Role::Button)
                        .tab_index(0)
                        .aria_label(group.name.clone())
                        .ml_1()
                        .mr_1()
                        .pr_1p5()
                        .pt(px(5.))
                        .pb_1p5()
                        .pl_1p5()
                        .rounded(ui::RADIUS_MD)
                        .border_1()
                        .border_color(ui::TRANSPARENT)
                        .focus_visible(|d| d.border_color(p.ring))
                        .flex()
                        .items_center()
                        .cursor_pointer()
                        .hover(|d| d.bg(p.worktree_sidebar_accent))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.toggle_bot_search(window, cx);
                            this.bot_ui.collapsed = false;
                            this.open_bot_group_on(&group_host, &gid, None, Some(window), cx);
                        }))
                        .child(div().mr_2().child(faces))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .flex_col()
                                .child(
                                    div()
                                        .truncate()
                                        .text_size(ui::TEXT_MD)
                                        .line_height(px(20.))
                                        .text_color(p.foreground)
                                        .child(marked(&group.name, &ranges, p)),
                                )
                                .when(!member_names.is_empty(), |d| {
                                    d.child(
                                        div()
                                            .truncate()
                                            .text_size(ui::TEXT_SM)
                                            .line_height(px(16.))
                                            .text_color(p.muted_foreground)
                                            .child(member_names.join(", ")),
                                    )
                                }),
                        ),
                );
            }
        }
        let note = |id: &'static str| {
            div()
                .id(id)
                .debug_selector(move || id.into())
                .px_3()
                .py_1()
                .flex()
                .flex_wrap()
                .items_center()
                .gap_x_1()
                .text_size(ui::TEXT_SM)
                .text_color(p.muted_foreground)
        };
        let link = |id: &'static str, label: &'static str| {
            div()
                .id(id)
                .debug_selector(move || id.into())
                .cursor_pointer()
                .text_color(p.primary)
                .hover(gpui::Styled::underline)
                .child(label)
        };
        if matches.is_empty() && group_matches == 0 && hidden_paused == 0 {
            list = list.child(
                note("bots-search-empty")
                    .child(text::SB_NO_MATCHES.get())
                    .child("·")
                    .child(
                        link("bots-search-clear", text::SB_CLEAR_SEARCH.get()).on_click(
                            cx.listener(|this, _, window, cx| this.clear_bot_search(window, cx)),
                        ),
                    ),
            );
        }
        if hidden_paused > 0 {
            list = list.child(
                note("bots-search-paused")
                    .child(text::SB_PAUSED_MATCHES.format(&[("n", &hidden_paused.to_string())]))
                    .child("·")
                    .child(
                        link("bots-search-show-paused", text::SB_SHOW.get()).on_click(cx.listener(
                            |this, _, _, cx| {
                                let mut v = this.bot_view(cx);
                                v.hide_paused = false;
                                this.set_bot_view(v, cx);
                            },
                        )),
                    )
                    .when(matches.is_empty() && group_matches == 0, |d| {
                        d.child("·").child(
                            link("bots-search-clear", text::SB_CLEAR_SEARCH.get()).on_click(
                                cx.listener(|this, _, window, cx| {
                                    this.clear_bot_search(window, cx);
                                }),
                            ),
                        )
                    }),
            );
        }
        Some(field.child(list))
    }

    fn clear_bot_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some((input, _)) = &self.bot_ui.search {
            input.update(cx, |s, cx| {
                s.set_value("", window, cx);
                s.focus(window, cx);
            });
        }
        self.bot_ui.query.clear();
        cx.notify();
    }

    /// Orca `BotSearchResultRow` (`#46830`): 32 px picture with status badge, name with marks,
    /// inset focus ring staying inside the list, and matched purpose / chat title or body snippet.
    fn render_bot_search_result(
        &self,
        bot: &SidebarBot,
        facts: &super::rows::BotFacts,
        m: &view::SearchMatch,
        p: &'static ui::Palette,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let detail: Option<gpui::Div> = if let Some((snippet, ranges)) = &m.purpose {
            Some(marked(snippet, ranges, p))
        } else if let Some((chat_title, snippet, ranges)) = &m.chat_hit {
            let show_prefix = chat_title != snippet;
            Some(
                div()
                    .flex()
                    .min_w_0()
                    .items_center()
                    .gap_1()
                    .overflow_hidden()
                    .child(ui::icons::svg_box(
                        ui::icons::MESSAGE_SQUARE_SVG,
                        11.,
                        p.muted_foreground,
                    ))
                    .when(show_prefix, |d| {
                        d.child(
                            div()
                                .flex_none()
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(p.foreground)
                                .child(chat_title.clone()),
                        )
                        .child("·")
                    })
                    .child(marked(snippet, ranges, p)),
            )
        } else if !bot.description.is_empty() {
            Some(marked(&bot.description, &[], p))
        } else {
            None
        };
        let stop_tip = facts.limit.as_ref().map(|l| l.tip(&bot.agent_id));
        let badge_status = if facts.status == super::rows::RowStatus::Inactive {
            super::rows::RowStatus::Active
        } else {
            facts.status
        };
        let host = bot.host.clone();
        let id = bot.id.clone();
        let sel = format!("bot-search-result-{}", bot.id.as_str());
        let elem_id = if bot.host.is_local() {
            sel.clone()
        } else {
            format!(
                "bot-search-result-{}-{}",
                bot.host.as_key(),
                bot.id.as_str()
            )
        };
        div()
            .id(SharedString::from(elem_id))
            .debug_selector(move || sel.clone())
            .role(gpui::Role::Button)
            .tab_index(0)
            .aria_label(bot.name.clone())
            // Orca #46830: card surface + inset focus ring kept inside the scroll list.
            .ml_1()
            .mr_1()
            .pr_1p5()
            .pt(px(5.))
            .pb_1p5()
            .pl_1p5()
            .rounded(ui::RADIUS_MD)
            .border_1()
            .border_color(ui::TRANSPARENT)
            .focus_visible(|d| d.border_color(p.ring))
            .flex()
            .items_center()
            .cursor_pointer()
            .hover(|d| d.bg(p.worktree_sidebar_accent))
            .on_click(cx.listener(move |this, _, window, cx| {
                this.toggle_bot_search(window, cx);
                this.bot_ui.collapsed = false;
                this.open_bot_home_on(&host, id.clone(), None, cx);
            }))
            .child(
                div()
                    .relative()
                    .mr_2()
                    .flex_none()
                    .when(bot.paused, |d| d.opacity(0.6))
                    .child(self.bot_avatar(bot, super::rows::AVATAR, ui::RADIUS_SM, p))
                    .children(super::rows::avatar_badge(
                        badge_status,
                        stop_tip,
                        p.worktree_sidebar,
                        p,
                        cx,
                    )),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .truncate()
                            .text_size(ui::TEXT_MD)
                            .line_height(px(20.))
                            .text_color(if bot.paused {
                                p.muted_foreground
                            } else {
                                p.foreground
                            })
                            .child(marked(&bot.name, &m.name_ranges, p)),
                    )
                    .when(bot.paused || detail.is_some(), |d| {
                        d.child(
                            div()
                                .flex()
                                .min_w_0()
                                .overflow_hidden()
                                .text_size(ui::TEXT_SM)
                                .line_height(px(16.))
                                .text_color(p.muted_foreground)
                                .when(bot.paused, |d| d.child(text::PAUSED_BADGE.get()))
                                .when(bot.paused && detail.is_some(), |d| d.child(" · "))
                                .children(detail),
                        )
                    }),
            )
    }
}

/// Orca `BotPreviewPip` (`#47884`): status hint pip on a fully-visible collapsed face.
fn render_preview_pip(
    bot_id: &str,
    signal: Option<view::PreviewSignal>,
    spinners: &ui::status::SpinnerSlots,
    p: &'static ui::Palette,
    cx: &gpui::App,
) -> Option<gpui::AnyElement> {
    match signal? {
        view::PreviewSignal::Attention => {
            let pip_sel = format!("bots-collapsed-waiting-{bot_id}");
            Some(
                div()
                    .debug_selector(move || pip_sel.clone())
                    .absolute()
                    .right(px(-3.))
                    .bottom(px(-3.))
                    .size(px(14.))
                    .rounded_full()
                    .bg(p.worktree_sidebar)
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(ui::status::status_glyph(
                        krill_proto::control::AgentState::Waiting,
                        false,
                        ui::status::Size::Sm,
                        cx,
                    ))
                    .into_any_element(),
            )
        }
        view::PreviewSignal::Unread => {
            let pip_sel = format!("bots-collapsed-unread-{bot_id}");
            Some(
                ui::status::unread_alert_dot(p.worktree_sidebar, cx)
                    .debug_selector(move || pip_sel.clone())
                    .absolute()
                    .right(px(-2.))
                    .bottom(px(-2.))
                    .into_any_element(),
            )
        }
        view::PreviewSignal::Working => {
            let pip_sel = format!("bots-collapsed-working-{bot_id}");
            Some(
                div()
                    .debug_selector(move || pip_sel.clone())
                    .absolute()
                    .right(px(-3.))
                    .bottom(px(-3.))
                    .size(px(12.))
                    .rounded_full()
                    .bg(p.worktree_sidebar)
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(ui::status::status_glyph_slot(
                        krill_proto::control::AgentState::Working,
                        false,
                        ui::status::Size::Sm,
                        Some(spinners),
                        cx,
                    ))
                    .into_any_element(),
            )
        }
    }
}

/// Orca `BotPreviewEdge` (`#47622`, `#47884`): left/right overflow paging button with direction
/// chevron, hidden count badge, tooltip, and edge status hint pip.
fn render_preview_edge(
    direction: i8,
    count: usize,
    attention: usize,
    signal: Option<view::PreviewSignal>,
    p: &'static ui::Palette,
    on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut gpui::App) + 'static,
    cx: &gpui::App,
) -> gpui::Stateful<gpui::Div> {
    let sel = if direction < 0 {
        "bots-collapsed-edge-left"
    } else {
        "bots-collapsed-edge-right"
    };
    let action = if direction < 0 {
        text::SB_SCROLL_LEFT.format(&[("n", &count.to_string())])
    } else {
        text::SB_SCROLL_RIGHT.format(&[("n", &count.to_string())])
    };
    let hint = if attention > 0 {
        Some(if attention == 1 {
            text::SB_NEEDS_YOU_ONE.format(&[("n", "1")])
        } else {
            text::SB_NEEDS_YOU_MANY.format(&[("n", &attention.to_string())])
        })
    } else if signal == Some(view::PreviewSignal::Unread) {
        Some(text::SB_UNREAD.get().to_owned())
    } else if signal == Some(view::PreviewSignal::Working) {
        Some(text::STATUS_WORKING.get().to_owned())
    } else {
        None
    };
    let tip: SharedString = match hint {
        Some(h) => format!("{action} · {h}").into(),
        None => action.into(),
    };
    let tip_tt = tip.clone();
    let chevron = if direction < 0 {
        gpui_kit_assets::IconName::ChevronLeft
    } else {
        gpui_kit_assets::IconName::ChevronRight
    };
    let mut edge_pip: Option<gpui::AnyElement> = match signal {
        Some(view::PreviewSignal::Attention) => Some(
            div()
                .flex_none()
                .size(px(11.))
                .rounded_full()
                .flex()
                .items_center()
                .justify_center()
                .child(ui::status::status_glyph(
                    krill_proto::control::AgentState::Waiting,
                    false,
                    ui::status::Size::Sm,
                    cx,
                ))
                .into_any_element(),
        ),
        Some(view::PreviewSignal::Unread) => Some(
            div()
                .flex_none()
                .flex()
                .items_center()
                .child(ui::status::unread_alert_dot(p.worktree_sidebar, cx))
                .into_any_element(),
        ),
        Some(view::PreviewSignal::Working) => Some(
            div()
                .flex_none()
                .size(px(11.))
                .flex()
                .items_center()
                .justify_center()
                .child(
                    gpui_component::Icon::new(gpui_kit_assets::IconName::CircleDashed)
                        .size(px(10.))
                        .text_color(p.agent_working()),
                )
                .into_any_element(),
        ),
        None => None,
    };
    div()
        .id(sel)
        .debug_selector(move || sel.into())
        .role(gpui::Role::Button)
        .aria_label(tip)
        .flex_none()
        .h_full()
        .px_1()
        .rounded(ui::RADIUS_XS)
        .bg(p.worktree_sidebar.opacity(0.92))
        .text_color(p.worktree_sidebar_foreground.opacity(0.85))
        .hover(|d| {
            d.bg(p.worktree_sidebar_accent)
                .text_color(p.worktree_sidebar_foreground)
        })
        .cursor_pointer()
        .flex()
        .items_center()
        .justify_center()
        .gap_0p5()
        .tooltip(move |window, cx| {
            gpui_component::tooltip::Tooltip::new(tip_tt.clone()).build(window, cx)
        })
        .on_click(on_click)
        .when(direction < 0, |d| {
            d.child(gpui_component::Icon::new(chevron).size(px(10.)))
                .child(
                    div()
                        .text_size(ui::TEXT_3XS)
                        .line_height(gpui::relative(1.))
                        .child(count.to_string()),
                )
                .children(edge_pip.take())
        })
        .when(direction > 0, |d| {
            d.children(edge_pip.take())
                .child(
                    div()
                        .text_size(ui::TEXT_3XS)
                        .line_height(gpui::relative(1.))
                        .child(format!("+{count}")),
                )
                .child(gpui_component::Icon::new(chevron).size(px(10.)))
        })
}

/// Text with Orca's find-match marks (`bg-search-match/25`) on `ranges`.
fn marked(text: &str, ranges: &[view::SearchRange], p: &'static ui::Palette) -> gpui::Div {
    let mut row = div().flex().min_w_0().overflow_hidden().whitespace_nowrap();
    let mut at = 0;
    for r in ranges {
        if r.start > at {
            row = row.child(text.get(at..r.start).unwrap_or_default().to_owned());
        }
        row = row.child(
            div()
                .rounded(px(2.))
                .bg(p.warning.opacity(0.25))
                .text_color(p.foreground)
                .child(text.get(r.clone()).unwrap_or_default().to_owned()),
        );
        at = r.end;
    }
    if at < text.len() {
        row = row.child(
            div()
                .min_w_0()
                .truncate()
                .child(text.get(at..).unwrap_or_default().to_owned()),
        );
    }
    row
}

fn bot_options_menu(
    menu: PopupMenu,
    this: &gpui::WeakEntity<Shell>,
    window: &mut Window,
    cx: &mut Context<PopupMenu>,
) -> PopupMenu {
    if let Some(shell) = this.upgrade() {
        cx.observe(&shell, |_, _, cx| cx.notify()).detach();
    }
    let menu_weak = cx.weak_entity();
    let sort_shell = this.clone();
    let sort_menu = PopupMenu::build(window, cx, move |menu, _, cx| {
        if let Some(shell) = sort_shell.upgrade() {
            cx.observe(&shell, |_, _, cx| cx.notify()).detach();
        }
        let shell = sort_shell.clone();
        let mn = cx.weak_entity();
        menu.min_w(px(260.)).item(
            PopupMenuItem::element(move |_, cx| {
                let p = ui::palette(cx);
                let current = shell
                    .upgrade()
                    .map(|s| s.read(cx).bot_view(cx).sort)
                    .unwrap_or_default();
                let mut panel = menu_parts::submenu_panel_frame("bots-sort-panel", p);
                for sort in BotSidebarSort::ALL {
                    let shell = shell.clone();
                    let mn = mn.clone();
                    let sel = format!("bots-sort-{sort:?}").to_lowercase();
                    panel = panel.child(
                        div()
                            .id(SharedString::from(sel.clone()))
                            .debug_selector(move || sel.clone())
                            .px_2()
                            .py_1()
                            .rounded(ui::RADIUS_XS)
                            .cursor_pointer()
                            .flex()
                            .items_start()
                            .gap_1p5()
                            .hover(move |s| s.bg(p.accent).text_color(p.accent_foreground))
                            .on_click(move |_, _, cx| {
                                shell
                                    .update(cx, |s, cx| {
                                        let mut v = s.bot_view(cx);
                                        v.sort = sort;
                                        s.set_bot_view(v, cx);
                                    })
                                    .ok();
                                mn.update(cx, |_, cx| cx.notify()).ok();
                            })
                            .child(
                                div()
                                    .pt(px(2.))
                                    .child(menu_parts::check_slot(current == sort, p)),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .flex()
                                    .flex_col()
                                    .child(
                                        div()
                                            .text_size(ui::TEXT_XS)
                                            .text_color(p.popover_foreground)
                                            .child(sort.label()),
                                    )
                                    .when_some(sort.description(), |d, desc| {
                                        d.child(
                                            div()
                                                .whitespace_normal()
                                                .text_size(ui::TEXT_2XS)
                                                .text_color(p.muted_foreground)
                                                .child(desc),
                                        )
                                    }),
                            ),
                    );
                }
                panel
            })
            .disabled(true),
        )
    });
    let label_shell = this.clone();
    let paused_shell = this.clone();
    let paused_menu = menu_weak.clone();
    let sleep_shell = this.clone();
    let sleep_menu = menu_weak;
    menu.min_w(px(240.))
        .item(
            PopupMenuItem::element(|_, cx| {
                div()
                    .id("bots-options-menu")
                    .debug_selector(|| "bots-options-menu".into())
                    .py_1()
                    .text_size(ui::TEXT_XS)
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(ui::palette(cx).popover_foreground)
                    .child(text::SBO_TITLE.get())
            })
            .disabled(true),
        )
        .item(
            PopupMenuItem::submenu(text::SBO_SORT_BY.get(), sort_menu).submenu_label(
                move |_, cx| {
                    let p = ui::palette(cx);
                    let current = label_shell
                        .upgrade()
                        .map(|s| s.read(cx).bot_view(cx).sort)
                        .unwrap_or_default();
                    div()
                        .id("bots-submenu-sort")
                        .debug_selector(|| "bots-submenu-sort".into())
                        .min_w_0()
                        .flex_1()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_2()
                        .text_size(ui::TEXT_XS)
                        .text_color(p.popover_foreground)
                        .child(text::SBO_SORT_BY.get())
                        .child(div().text_color(p.muted_foreground).child(current.label()))
                },
            ),
        )
        .separator()
        .item(
            PopupMenuItem::element(move |_, cx| {
                let p = ui::palette(cx);
                let v = paused_shell
                    .upgrade()
                    .map(|s| s.read(cx).bot_view(cx))
                    .unwrap_or_default();
                let (ps, pm) = (paused_shell.clone(), paused_menu.clone());
                let (ss, sm) = (sleep_shell.clone(), sleep_menu.clone());
                div()
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .child(menu_parts::section_label(text::SBO_FILTERS.get(), p))
                    .child(menu_parts::render_filter_toggle_row(
                        "bots-filter-hide-paused",
                        PAUSE_SVG,
                        text::SBO_HIDE_PAUSED.get(),
                        v.hide_paused,
                        false,
                        p,
                        move |cx| {
                            ps.update(cx, |s, cx| {
                                let mut v = s.bot_view(cx);
                                v.hide_paused = !v.hide_paused;
                                s.set_bot_view(v, cx);
                            })
                            .ok();
                            pm.update(cx, |_, cx| cx.notify()).ok();
                        },
                    ))
                    .child(menu_parts::render_filter_toggle_row(
                        "bots-filter-hide-sleeping",
                        ui::icons::MOON_SVG,
                        text::SBO_HIDE_SLEEPING.get(),
                        v.hide_sleeping_workers,
                        false,
                        p,
                        move |cx| {
                            ss.update(cx, |s, cx| {
                                let mut v = s.bot_view(cx);
                                v.hide_sleeping_workers = !v.hide_sleeping_workers;
                                s.set_bot_view(v, cx);
                            })
                            .ok();
                            sm.update(cx, |_, cx| cx.notify()).ok();
                        },
                    ))
            })
            .disabled(true),
        )
}

/// Lucide `pause` (Orca's "Hide paused bots" glyph).
const PAUSE_SVG: &[u8] = br#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" xmlns="http://www.w3.org/2000/svg"><rect x="14" y="4" width="4" height="16" rx="1"/><rect x="6" y="4" width="4" height="16" rx="1"/></svg>"#;
