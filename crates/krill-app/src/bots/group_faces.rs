// Adapted from Orca (Copyright (c) 2026 Lovecast Inc., MIT License).
//! Overlapping face clusters for multi-bot group rows, headers, tabs, and the New/Manage
//! group dialog (`BotGroupFaces`, `BotGroupFaceCluster`, `BotGroupTabIcon`,
//! `bot-group-face-layout`).

use std::sync::Arc;

use gpui::{AnyElement, FontWeight, Image, Pixels, div, prelude::*, px};
use krill_proto::bots::BotId;

use super::{SidebarBot, avatar};
use crate::ui;

/// Base canvas size (`BotGroupFaces` / `BOT_GROUP_FACE_SLOT = 28`).
pub const FACE_SLOT: f32 = 28.;
/// Cutout ring thickness at base `28px` slot (`BOT_GROUP_FACE_GAP = 1.5`).
const FACE_GAP: f32 = 1.5;

const TAB_FACE: f32 = 12.;
const TAB_FACE_STEP: f32 = 8.;
const TAB_BOX_HEIGHT: f32 = 14.;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FaceSpot {
    pub x: f32,
    pub y: f32,
    pub s: f32,
    pub font_size: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FaceLayout {
    pub spots: Vec<FaceSpot>,
    pub overflow: usize,
}

/// Computes Orca's exact 1..=3+ face positions in a `28x28` unit coordinate system.
#[must_use]
pub fn compute_face_layout(member_count: usize) -> FaceLayout {
    match member_count {
        0 => FaceLayout {
            spots: Vec::new(),
            overflow: 0,
        },
        1 => FaceLayout {
            spots: vec![FaceSpot {
                x: 4.,
                y: 4.,
                s: 20.,
                font_size: 11.,
            }],
            overflow: 0,
        },
        2 => FaceLayout {
            spots: vec![
                FaceSpot {
                    x: 11.,
                    y: 0.,
                    s: 17.,
                    font_size: 10.,
                },
                FaceSpot {
                    x: 0.,
                    y: 10.,
                    s: 18.,
                    font_size: 10.,
                },
            ],
            overflow: 0,
        },
        3 => FaceLayout {
            spots: vec![
                FaceSpot {
                    x: 14.,
                    y: 0.,
                    s: 14.,
                    font_size: 8.,
                },
                FaceSpot {
                    x: 0.,
                    y: 3.,
                    s: 15.,
                    font_size: 9.,
                },
                FaceSpot {
                    x: 5.,
                    y: 15.,
                    s: 13.,
                    font_size: 8.,
                },
            ],
            overflow: 0,
        },
        n => FaceLayout {
            spots: vec![
                FaceSpot {
                    x: 14.,
                    y: 0.,
                    s: 14.,
                    font_size: 8.,
                },
                FaceSpot {
                    x: 0.,
                    y: 3.,
                    s: 15.,
                    font_size: 9.,
                },
                FaceSpot {
                    x: 5.,
                    y: 15.,
                    s: 13.,
                    font_size: 7.,
                },
            ],
            overflow: n.saturating_sub(2),
        },
    }
}

/// One resolved member face ready to render in a group cluster.
#[derive(Debug, Clone)]
pub struct FaceMember<'a> {
    pub bot: &'a SidebarBot,
    pub upload: Option<Arc<Image>>,
}

/// Resolves group `bot_ids` into ordered [`FaceMember`] entries from the known sidebar bots.
#[must_use]
pub fn resolve_members<'a>(
    bot_ids: &[BotId],
    bots: &'a [SidebarBot],
    uploads: &dyn Fn(&BotId) -> Option<Arc<Image>>,
) -> Vec<FaceMember<'a>> {
    bot_ids
        .iter()
        .filter_map(|id| {
            bots.iter().find(|b| &b.id == id).map(|bot| FaceMember {
                bot,
                upload: uploads(&bot.id),
            })
        })
        .collect()
}

fn face_text_size(font_px: f32) -> gpui::Rems {
    if font_px >= 20. {
        ui::TEXT_2XL
    } else if font_px >= 16. {
        ui::TEXT_XL
    } else if font_px >= 13. {
        ui::TEXT_LG
    } else if font_px >= 11. {
        ui::TEXT_SM
    } else {
        ui::TEXT_2XS
    }
}

fn render_tile_or_fallback(
    member: &FaceMember<'_>,
    size_px: f32,
    font_px: f32,
    p: &ui::Palette,
) -> AnyElement {
    let radius = px(size_px / 2.);
    let inner = match &member.bot.avatar {
        avatar::Avatar::Mascot { .. } | avatar::Avatar::Upload => avatar::render(
            &member.bot.avatar,
            member.upload.clone(),
            &member.bot.name,
            px(size_px),
            radius,
            p,
        ),
        avatar::Avatar::Emoji(glyph) | avatar::Avatar::Initial(glyph) => div()
            .flex_none()
            .size(px(size_px))
            .rounded(radius)
            .bg(p.muted)
            .flex()
            .items_center()
            .justify_center()
            .text_size(face_text_size(font_px))
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(p.foreground)
            .child(glyph.clone())
            .into_any_element(),
    };
    div()
        .flex_none()
        .size(px(size_px))
        .rounded_full()
        .overflow_hidden()
        .child(inner)
        .into_any_element()
}

/// Renders Orca's clustered group faces at `slot_size` (`28`, `36`, `112`, ...), separated by
/// a ring colored `ring_bg` (the underlying sidebar/surface color).
#[must_use]
pub fn render_group_faces(
    members: &[FaceMember<'_>],
    slot_size: f32,
    ring_bg: gpui::Hsla,
    p: &ui::Palette,
) -> AnyElement {
    let layout = compute_face_layout(members.len());
    let scale = slot_size / FACE_SLOT;
    let ring_px = (FACE_GAP * scale).max(1.);
    let ring = px(ring_px);
    let mut container = div().relative().flex_none().size(px(slot_size));

    if layout.spots.is_empty() {
        return container
            .child(
                div()
                    .size(px(slot_size))
                    .rounded_full()
                    .border_1()
                    .border_dashed()
                    .border_color(p.border)
                    .bg(p.muted.opacity(0.35)),
            )
            .into_any_element();
    }

    let avatar_slots = if layout.overflow > 0 {
        layout.spots.len().saturating_sub(1)
    } else {
        layout.spots.len()
    };

    for (idx, spot) in layout.spots.iter().take(avatar_slots).enumerate() {
        if let Some(member) = members.get(idx) {
            let s_px = spot.s * scale;
            let f_px = spot.font_size * scale;
            let outer_radius = px(s_px / 2. + ring_px);
            let inner = render_tile_or_fallback(member, s_px, f_px, p);
            container = container.child(
                div()
                    .absolute()
                    .left(px(spot.x * scale))
                    .top(px(spot.y * scale))
                    .rounded(outer_radius)
                    .overflow_hidden()
                    .border(ring)
                    .border_color(ring_bg)
                    .child(inner),
            );
        }
    }

    if layout.overflow > 0
        && let Some(spot) = layout.spots.last()
    {
        let s_px = spot.s * scale;
        let f_px = spot.font_size * scale;
        let outer_radius = px(s_px / 2. + ring_px);
        container = container.child(
            div()
                .absolute()
                .left(px(spot.x * scale))
                .top(px(spot.y * scale))
                .rounded(outer_radius)
                .overflow_hidden()
                .border(ring)
                .border_color(ring_bg)
                .child(
                    div()
                        .size(px(s_px))
                        .rounded(px(s_px / 2.))
                        .bg(p.muted)
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_size(face_text_size(f_px))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(p.muted_foreground)
                        .child(format!("+{}", layout.overflow)),
                ),
        );
    }

    container.into_any_element()
}

/// Orca `BotGroupTabIcon`: up to 2 overlapping `12px` circular member avatars for workspace tabs.
#[must_use]
pub fn render_group_tab_icon(
    members: &[FaceMember<'_>],
    ring_bg: gpui::Hsla,
    p: &ui::Palette,
) -> AnyElement {
    let shown: Vec<&FaceMember<'_>> = members.iter().take(2).collect();
    if shown.is_empty() {
        return ui::icons::svg_icon(ui::icons::USERS_SVG, ui::ICON_SM, p.muted_foreground)
            .into_any_element();
    }
    let width: Pixels = px(if shown.len() == 1 {
        TAB_FACE + 2.
    } else {
        TAB_FACE + TAB_FACE_STEP + 2.
    });
    let mut box_div = div()
        .flex_none()
        .w(width)
        .h(px(TAB_BOX_HEIGHT))
        .flex()
        .items_start();
    for (idx, member) in shown.into_iter().enumerate() {
        let mut slot = div()
            .flex_none()
            .size(px(TAB_FACE + 2.))
            .rounded_full()
            .overflow_hidden()
            .border_1()
            .border_color(ring_bg)
            .flex()
            .items_center()
            .justify_center()
            .child(render_tile_or_fallback(member, TAB_FACE, 7., p));
        if idx > 0 {
            slot = slot.ml(px(TAB_FACE_STEP - (TAB_FACE + 2.))).mt(px(2.));
        }
        box_div = box_div.child(slot);
    }
    box_div.into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn face_layout_matches_orca_slot_geometry() {
        assert!(compute_face_layout(0).spots.is_empty());
        let one = compute_face_layout(1);
        assert_eq!(one.spots.len(), 1);
        assert_eq!(one.overflow, 0);
        assert!((one.spots[0].s - 20.).abs() < f32::EPSILON);

        let two = compute_face_layout(2);
        assert_eq!(two.spots.len(), 2);
        assert_eq!(two.overflow, 0);

        let three = compute_face_layout(3);
        assert_eq!(three.spots.len(), 3);
        assert_eq!(three.overflow, 0);

        let five = compute_face_layout(5);
        assert_eq!(five.spots.len(), 3);
        assert_eq!(five.overflow, 3);
        assert!((five.spots[2].font_size - 7.).abs() < f32::EPSILON);
    }
}
