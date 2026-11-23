// Image tiles and viewer adapted from Orca NativeChatImageAttachmentPreview.tsx,
// NativeChatImageAttachments.tsx and PanZoomImageDialog.tsx (MIT).
// Copyright (c) 2026 Lovecast Inc.
//! Chat images: thumbnails in the composer and in sent messages, and the full-size viewer.
//!
//! Pictures are decoded on the background executor into small BGRA frames so a row of 5K
//! screenshots costs kilobytes, not a full decode each. The viewer decodes one picture at a
//! time, capped at [`VIEWER_MAX_SIDE`], and hands its texture back when it closes.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, Context, InteractiveElement as _, IntoElement as _, KeyDownEvent, MouseButton,
    ObjectFit, ParentElement as _, Pixels, RenderImage, SharedString,
    StatefulInteractiveElement as _, Styled as _, StyledImage as _, Window, div, img, px,
};
use gpui_component::IconName;
use gpui_component::Sizable as _;
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::{Disableable as _, Icon};
use krill_app::i18n::catalog::chat as ctext;
use krill_app::ui;
use krill_proto::agent_history::{ChatItem, ChatItemKind};

use super::Shell;

/// Composer thumbnail side (Orca `size-14`).
pub(super) const COMPOSER_TILE: Pixels = px(56.);
/// Sent-message thumbnail side (Orca `NativeChatImageTile` `size-20`).
pub(super) const MESSAGE_TILE: Pixels = px(80.);
/// Thumbnails are decoded at twice the largest tile for Retina screens.
const THUMB_SIDE: u32 = 160;
/// The viewer never holds more than this many pixels on a side.
const VIEWER_MAX_SIDE: u32 = 2560;
const MAX_IMAGE_BYTES: u64 = 64 * 1024 * 1024;
/// ponytail: the cache is cleared whole past this; an LRU if long chats re-decode visibly.
const MAX_CACHED_THUMBS: usize = 256;

#[derive(Clone)]
pub(super) enum Loaded {
    Loading,
    Ready(Arc<RenderImage>),
    Failed,
}

pub(super) struct ImageViewer {
    pub paths: Vec<PathBuf>,
    pub numbers: Vec<usize>,
    pub index: usize,
    pub image: Loaded,
    focus: Option<gpui::FocusHandle>,
    /// Scale over the fitted picture (Orca `usePanZoomViewport`); 1 is fitted.
    pub zoom: f32,
    pub pan: gpui::Point<Pixels>,
    /// Where the last drag move was, while the picture is being dragged.
    drag: Option<gpui::Point<Pixels>>,
    copied_at: Option<std::time::Instant>,
}

/// Orca `IMAGE_VIEWER_ZOOM_STEP`.
const ZOOM_STEP: f32 = 1.25;
const MIN_ZOOM: f32 = 0.25;
const MAX_ZOOM: f32 = 8.;

impl ImageViewer {
    fn fitted(&self) -> bool {
        (self.zoom - 1.).abs() < f32::EPSILON && self.pan == gpui::Point::default()
    }

    fn zoom_by(&mut self, factor: f32) {
        self.zoom = (self.zoom * factor).clamp(MIN_ZOOM, MAX_ZOOM);
        if (self.zoom - 1.).abs() < 0.01 {
            self.zoom = 1.;
        }
    }

    fn fit(&mut self) {
        self.zoom = 1.;
        self.pan = gpui::Point::default();
    }
}

/// PNG bytes of the picture at `path` for the clipboard (Orca `copyImageAsPng`).
fn png_bytes(path: &Path) -> Option<Vec<u8>> {
    if std::fs::metadata(path).ok()?.len() > MAX_IMAGE_BYTES {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Some(bytes);
    }
    let picture = image::load_from_memory(&bytes).ok()?;
    let mut out = std::io::Cursor::new(Vec::new());
    picture.write_to(&mut out, image::ImageFormat::Png).ok()?;
    Some(out.into_inner())
}

#[derive(Default)]
pub(crate) struct ChatImages {
    thumbs: HashMap<PathBuf, Loaded>,
    pub(super) viewer: Option<ImageViewer>,
}

#[cfg(all(test, unix))]
impl ChatImages {
    pub(super) fn thumbs_for_test(&self, path: &Path) -> Option<&Loaded> {
        self.thumbs.get(path)
    }
}

/// Image paths krilld attached to a user prompt (one per line of `file_path`).
pub(super) fn item_image_paths(item: &ChatItem) -> Vec<PathBuf> {
    if item.kind != ChatItemKind::User {
        return Vec::new();
    }
    item.file_path
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(PathBuf::from)
        .collect()
}

/// The numbers the agent gave a prompt's images (`[Image #3]`), so a tile's badge matches the
/// prompt text. Without a marker for every image they count from 1, as a draft does.
pub(super) fn image_numbers(text: &str, count: usize) -> Vec<usize> {
    let mut found = Vec::new();
    let mut rest = text;
    while let Some(at) = rest.find("[Image #") {
        let after = rest.get(at + "[Image #".len()..).unwrap_or("");
        let digits: String = after.chars().take_while(char::is_ascii_digit).collect();
        if after
            .get(digits.len()..)
            .is_some_and(|tail| tail.starts_with(']'))
            && let Ok(n) = digits.parse::<usize>()
            && !found.contains(&n)
        {
            found.push(n);
        }
        rest = after;
    }
    if found.len() == count {
        found
    } else {
        (1..=count).collect()
    }
}

fn decode(path: &Path, max_side: u32, square: bool) -> Option<RenderImage> {
    use image::ImageDecoder as _;
    let file = std::fs::File::open(path).ok()?;
    if file.metadata().ok()?.len() > MAX_IMAGE_BYTES {
        return None;
    }
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(32_768);
    limits.max_image_height = Some(32_768);
    limits.max_alloc = Some(512 * 1024 * 1024);
    let mut reader = image::ImageReader::new(std::io::BufReader::new(file))
        .with_guessed_format()
        .ok()?;
    reader.limits(limits);
    let mut decoder = reader.into_decoder().ok()?;
    let orientation = decoder
        .orientation()
        .unwrap_or(image::metadata::Orientation::NoTransforms);
    let mut picture = image::DynamicImage::from_decoder(decoder).ok()?;
    picture.apply_orientation(orientation);
    let filter = image::imageops::FilterType::Triangle;
    let picture = if square {
        // Orca draws thumbnails `object-cover`: fill the square, crop the overflow.
        picture.resize_to_fill(max_side, max_side, filter)
    } else if picture.width() > max_side || picture.height() > max_side {
        picture.resize(max_side, max_side, filter)
    } else {
        picture
    };
    let mut rgba = picture.to_rgba8();
    // gpui keeps frames in BGRA.
    for pixel in rgba.as_chunks_mut::<4>().0 {
        pixel.swap(0, 2);
    }
    Some(RenderImage::new(vec![image::Frame::new(rgba)]))
}

/// One square image tile: the picture filling the box, an image icon while it loads, and an
/// optional `#n` badge in the bottom-left corner (Orca `ImageNumberBadge`).
pub(super) fn image_tile(
    id: SharedString,
    loaded: &Loaded,
    side: Pixels,
    number: Option<usize>,
    p: &ui::Palette,
) -> gpui::Stateful<gpui::Div> {
    let inner = div()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .overflow_hidden()
        .rounded(ui::RADIUS_MD)
        .border_1()
        .border_color(p.border)
        .bg(p.background)
        .hover(|s| s.border_color(p.ring))
        .child(match loaded {
            Loaded::Ready(image) => img(image.clone())
                .size_full()
                .object_fit(ObjectFit::Cover)
                .into_any_element(),
            Loaded::Loading | Loaded::Failed => Icon::default()
                .data(ui::icons::IMAGE_SVG)
                .size(px(ui::ICON_MD))
                .text_color(p.muted_foreground)
                .into_any_element(),
        });
    div()
        .id(id)
        .relative()
        .flex_none()
        .size(side)
        .cursor_pointer()
        .child(inner)
        .when_some(number, |d, n| {
            d.child(
                div()
                    .absolute()
                    .bottom(px(2.))
                    .left(px(2.))
                    .rounded_full()
                    .bg(p.background.opacity(0.85))
                    .px_1()
                    .text_size(ui::TEXT_2XS)
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(p.foreground)
                    .child(format!("#{n}")),
            )
        })
}

impl Shell {
    /// The thumbnail for `path`, starting its decode the first time it is asked for.
    pub(super) fn chat_thumbnail(&mut self, path: &Path, cx: &mut Context<Self>) -> Loaded {
        if let Some(loaded) = self.chat.images.thumbs.get(path) {
            return loaded.clone();
        }
        if self.chat.images.thumbs.len() >= MAX_CACHED_THUMBS {
            for (_, loaded) in self.chat.images.thumbs.drain() {
                if let Loaded::Ready(image) = loaded {
                    cx.drop_image(image, None);
                }
            }
        }
        self.chat
            .images
            .thumbs
            .insert(path.to_path_buf(), Loaded::Loading);
        let owned = path.to_path_buf();
        cx.spawn(async move |this, cx| {
            let read = owned.clone();
            let decoded = cx
                .background_executor()
                .spawn(async move { decode(&read, THUMB_SIDE, true) })
                .await;
            this.update(cx, |this, cx| {
                let loaded = decoded.map_or(Loaded::Failed, |image| Loaded::Ready(Arc::new(image)));
                if let Some(Loaded::Ready(old)) = this.chat.images.thumbs.insert(owned, loaded) {
                    cx.drop_image(old, None);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        Loaded::Loading
    }

    /// A row of message image tiles that open the viewer (Orca `NativeChatImageAttachments`:
    /// `mb-2 flex flex-wrap gap-1.5`, 80 px tiles). Remote hosts' files are not readable
    /// here, so their tiles stay icons and do not open.
    pub(super) fn chat_message_images(
        &mut self,
        id_prefix: &str,
        paths: &[PathBuf],
        numbers: &[usize],
        readable: bool,
        cx: &mut Context<Self>,
    ) -> Option<gpui::Div> {
        if paths.is_empty() {
            return None;
        }
        let p = ui::palette(cx);
        let mut row = div().flex().flex_wrap().gap_1p5();
        for (index, path) in paths.iter().enumerate() {
            let loaded = if readable {
                self.chat_thumbnail(path, cx)
            } else {
                Loaded::Failed
            };
            let number = numbers.get(index).copied();
            let label = number.map_or_else(
                || ctext::IMAGE_VIEW.get().to_owned(),
                |n| ctext::IMAGE_NUMBER.format(&[("number", &n.to_string())]),
            );
            let open_paths = paths.to_vec();
            let open_numbers = numbers.to_vec();
            let tile = image_tile(
                SharedString::from(format!("{id_prefix}-image-{index}")),
                &loaded,
                MESSAGE_TILE,
                number,
                p,
            )
            .debug_selector(move || format!("chat-message-image-{index}"))
            .tooltip(move |window, cx| {
                gpui_component::tooltip::Tooltip::new(label.clone()).build(window, cx)
            })
            .when(readable, |tile| {
                tile.on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.open_chat_image_viewer(
                            open_paths.clone(),
                            open_numbers.clone(),
                            index,
                            window,
                            cx,
                        );
                    }))
            });
            row = row.child(tile);
        }
        Some(row)
    }

    pub(super) fn open_chat_image_viewer(
        &mut self,
        paths: Vec<PathBuf>,
        numbers: Vec<usize>,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if index >= paths.len() {
            return;
        }
        self.close_chat_image_viewer(window, cx);
        self.chat.images.viewer = Some(ImageViewer {
            paths,
            numbers,
            index,
            image: Loaded::Loading,
            focus: None,
            zoom: 1.,
            pan: gpui::Point::default(),
            drag: None,
            copied_at: None,
        });
        self.load_chat_viewer_image(cx);
        cx.notify();
    }

    fn load_chat_viewer_image(&mut self, cx: &mut Context<Self>) {
        let Some(viewer) = self.chat.images.viewer.as_mut() else {
            return;
        };
        let Some(path) = viewer.paths.get(viewer.index).cloned() else {
            return;
        };
        let index = viewer.index;
        viewer.image = Loaded::Loading;
        cx.spawn(async move |this, cx| {
            let read = path.clone();
            let decoded = cx
                .background_executor()
                .spawn(async move { decode(&read, VIEWER_MAX_SIDE, false) })
                .await;
            this.update(cx, |this, cx| {
                // Only the picture still on show; a stepped-past or closed one is dropped.
                if let Some(viewer) = this.chat.images.viewer.as_mut()
                    && viewer.index == index
                    && viewer.paths.get(index) == Some(&path)
                    && matches!(viewer.image, Loaded::Loading)
                {
                    viewer.image =
                        decoded.map_or(Loaded::Failed, |image| Loaded::Ready(Arc::new(image)));
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// Previous (`-1`) or next (`1`) image of the message the viewer shows.
    pub(super) fn step_chat_image_viewer(
        &mut self,
        by: isize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(viewer) = self.chat.images.viewer.as_mut() else {
            return;
        };
        let Some(to) = viewer
            .index
            .checked_add_signed(by)
            .filter(|to| *to < viewer.paths.len())
        else {
            return;
        };
        if let Loaded::Ready(image) = std::mem::replace(&mut viewer.image, Loaded::Loading) {
            cx.drop_image(image, Some(window));
        }
        viewer.index = to;
        viewer.copied_at = None;
        viewer.fit();
        self.load_chat_viewer_image(cx);
        cx.notify();
    }

    /// Orca's Copy as PNG: the picture on show, as PNG, on the clipboard.
    fn copy_chat_image_png(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self
            .chat
            .images
            .viewer
            .as_ref()
            .and_then(|v| v.paths.get(v.index).cloned())
        else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let bytes = cx
                .background_executor()
                .spawn(async move { png_bytes(&path) })
                .await;
            let stamped = this
                .update(cx, |this, cx| {
                    let bytes = bytes?;
                    cx.write_to_clipboard(gpui::ClipboardItem::new_image(
                        &gpui::Image::from_bytes(gpui::ImageFormat::Png, bytes),
                    ));
                    let stamped = std::time::Instant::now();
                    if let Some(viewer) = this.chat.images.viewer.as_mut() {
                        viewer.copied_at = Some(stamped);
                    }
                    cx.notify();
                    Some(stamped)
                })
                .ok()
                .flatten();
            if let Some(stamped) = stamped {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(1500))
                    .await;
                this.update(cx, |this, cx| {
                    if let Some(viewer) = this.chat.images.viewer.as_mut()
                        && viewer.copied_at == Some(stamped)
                    {
                        viewer.copied_at = None;
                        cx.notify();
                    }
                })
                .ok();
            }
        })
        .detach();
    }

    pub(in crate::shell) fn has_chat_image_viewer(&self) -> bool {
        self.chat.images.viewer.is_some()
    }

    pub(in crate::shell) fn close_chat_image_viewer(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(viewer) = self.chat.images.viewer.take() else {
            return;
        };
        if let Loaded::Ready(image) = viewer.image {
            cx.drop_image(image, Some(window));
        }
        cx.notify();
    }

    /// The full-size viewer over the window (Orca `PanZoomImageDialog`): the picture fitted with
    /// 96 px clear of the edges, its name top-left, previous / next and close top-right.
    /// Esc or a click outside the picture closes it; ← → step through the message's images.
    pub(crate) fn render_chat_image_viewer(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let viewer = self.chat.images.viewer.as_mut()?;
        let focus = viewer
            .focus
            .get_or_insert_with(|| cx.focus_handle())
            .clone();
        if !focus.contains_focused(window, cx) {
            window.focus(&focus, cx);
        }
        let p = ui::palette(cx);
        let count = viewer.paths.len();
        let index = viewer.index;
        let title = viewer.numbers.get(index).map_or_else(
            || {
                viewer
                    .paths
                    .get(index)
                    .and_then(|path| path.file_name())
                    .map_or_else(String::new, |n| n.to_string_lossy().into_owned())
            },
            |n| ctext::IMAGE_NUMBER.format(&[("number", &n.to_string())]),
        );
        let viewport = window.viewport_size();
        // The picture fits 96px clear of the edges; zoomed, it is sized and placed by hand.
        let area = gpui::size(
            (viewport.width - px(192.)).max(px(1.)),
            (viewport.height - px(192.)).max(px(1.)),
        );
        let fitted = viewer.fitted();
        let (zoom, pan) = (viewer.zoom, viewer.pan);
        let grab = |el: gpui::Img| {
            el.debug_selector(|| "chat-image-viewer-picture".into())
                .id("chat-image-viewer-picture")
                .cursor_grab()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, e: &gpui::MouseDownEvent, _, cx| {
                        if let Some(viewer) = this.chat.images.viewer.as_mut() {
                            viewer.drag = Some(e.position);
                        }
                        cx.stop_propagation();
                    }),
                )
        };
        let picture = match &viewer.image {
            Loaded::Ready(image) if fitted => grab(img(image.clone()))
                .size_full()
                .object_fit(ObjectFit::ScaleDown)
                .into_any_element(),
            Loaded::Ready(image) => {
                let size = image.size(0);
                // Decoded sides are capped at `VIEWER_MAX_SIDE`, so they fit in f32 exactly.
                let side = |v: i32| f32::from(u16::try_from(v.max(1)).unwrap_or(u16::MAX));
                let (w, h) = (side(size.width.0), side(size.height.0));
                let fit = (f32::from(area.width) / w)
                    .min(f32::from(area.height) / h)
                    .min(1.);
                let (dw, dh) = (w * fit * zoom, h * fit * zoom);
                grab(img(image.clone()))
                    .absolute()
                    .left(px((f32::from(area.width) - dw) / 2.) + pan.x)
                    .top(px((f32::from(area.height) - dh) / 2.) + pan.y)
                    .w(px(dw))
                    .h(px(dh))
                    .into_any_element()
            }
            Loaded::Loading => Icon::default()
                .data(ui::icons::IMAGE_SVG)
                .size(px(ui::ICON_LG))
                .text_color(p.muted_foreground)
                .into_any_element(),
            Loaded::Failed => div()
                .flex()
                .items_center()
                .gap_2()
                .text_size(ui::TEXT_SM)
                .text_color(p.muted_foreground)
                .child(
                    Icon::default()
                        .data(ui::icons::IMAGE_SVG)
                        .size(px(ui::ICON_MD)),
                )
                .child(ctext::IMAGE_PREVIEW_UNAVAILABLE.get())
                .into_any_element(),
        };
        let tool = |id: &'static str, icon: IconName, label: &'static str| {
            Button::new(id)
                .debug_selector(move || id.into())
                .ghost()
                .small()
                .icon(icon)
                .tooltip(label)
                .accessibility_label(label)
        };
        let stepping = count > 1;
        let ready = matches!(viewer.image, Loaded::Ready(_));
        let copied = viewer
            .copied_at
            .is_some_and(|at| at.elapsed() < std::time::Duration::from_millis(1500));
        let toolbar = div()
            .absolute()
            .top(px(48.))
            .left(ui::SPACE_3)
            .right(ui::SPACE_3)
            .flex()
            .items_start()
            .gap_3()
            .child(
                div()
                    .id("chat-image-viewer-title")
                    .debug_selector(|| "chat-image-viewer-title".into())
                    .min_w_0()
                    .truncate()
                    .rounded(ui::RADIUS_MD)
                    .border_1()
                    .border_color(p.border)
                    .bg(p.background)
                    .px_2()
                    .py_1()
                    .text_size(ui::TEXT_SM)
                    .text_color(p.foreground)
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(title.clone()),
            )
            .child(
                div()
                    .id("chat-image-viewer-tools")
                    .ml_auto()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap_0p5()
                    .rounded(ui::RADIUS_MD)
                    .border_1()
                    .border_color(p.border)
                    .bg(p.background)
                    .p_0p5()
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .when(stepping, |d| {
                        d.child(
                            tool(
                                "chat-image-viewer-previous",
                                IconName::ChevronLeft,
                                ctext::IMAGE_PREVIOUS.get(),
                            )
                            .disabled(index == 0)
                            .on_click(cx.listener(
                                |this, _, window, cx| this.step_chat_image_viewer(-1, window, cx),
                            )),
                        )
                        .child(
                            tool(
                                "chat-image-viewer-next",
                                IconName::ChevronRight,
                                ctext::IMAGE_NEXT.get(),
                            )
                            .disabled(index + 1 >= count)
                            .on_click(cx.listener(
                                |this, _, window, cx| this.step_chat_image_viewer(1, window, cx),
                            )),
                        )
                        .child(div().mx_0p5().h_4().w(px(1.)).bg(p.border))
                    })
                    .child(
                        tool(
                            "chat-image-viewer-zoom-out",
                            IconName::Minus,
                            ctext::IMAGE_ZOOM_OUT.get(),
                        )
                        .icon(gpui_kit_assets::IconName::ZoomOut)
                        .disabled(!ready || zoom <= MIN_ZOOM)
                        .on_click(cx.listener(|this, _, _, cx| {
                            if let Some(viewer) = this.chat.images.viewer.as_mut() {
                                viewer.zoom_by(1. / ZOOM_STEP);
                            }
                            cx.notify();
                        })),
                    )
                    .child(
                        tool(
                            "chat-image-viewer-zoom-in",
                            IconName::Plus,
                            ctext::IMAGE_ZOOM_IN.get(),
                        )
                        .icon(gpui_kit_assets::IconName::ZoomIn)
                        .disabled(!ready || zoom >= MAX_ZOOM)
                        .on_click(cx.listener(|this, _, _, cx| {
                            if let Some(viewer) = this.chat.images.viewer.as_mut() {
                                viewer.zoom_by(ZOOM_STEP);
                            }
                            cx.notify();
                        })),
                    )
                    .child(
                        tool(
                            "chat-image-viewer-fit",
                            IconName::Maximize,
                            ctext::IMAGE_FIT.get(),
                        )
                        .disabled(!ready || fitted)
                        .on_click(cx.listener(|this, _, _, cx| {
                            if let Some(viewer) = this.chat.images.viewer.as_mut() {
                                viewer.fit();
                            }
                            cx.notify();
                        })),
                    )
                    .child(
                        tool(
                            "chat-image-viewer-copy",
                            if copied {
                                IconName::Check
                            } else {
                                IconName::Copy
                            },
                            if copied {
                                ctext::IMAGE_COPIED_PNG.get()
                            } else {
                                ctext::IMAGE_COPY_PNG.get()
                            },
                        )
                        .disabled(!ready)
                        .on_click(cx.listener(|this, _, _, cx| this.copy_chat_image_png(cx))),
                    )
                    .child(
                        tool(
                            "chat-image-viewer-close",
                            IconName::Close,
                            ctext::IMAGE_CLOSE.get(),
                        )
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.close_chat_image_viewer(window, cx);
                        })),
                    ),
            );
        let surface = div()
            .id("chat-image-viewer")
            .debug_selector(|| "chat-image-viewer".into())
            .role(gpui::Role::Dialog)
            .aria_label(title)
            .relative()
            .size_full()
            .on_key_down(cx.listener(|this, e: &KeyDownEvent, window, cx| {
                let m = e.keystroke.modifiers;
                if m.alt || m.platform || m.control {
                    return;
                }
                match e.keystroke.key.as_str() {
                    "escape" => this.close_chat_image_viewer(window, cx),
                    "left" => this.step_chat_image_viewer(-1, window, cx),
                    "right" => this.step_chat_image_viewer(1, window, cx),
                    _ => return,
                }
                cx.stop_propagation();
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| this.close_chat_image_viewer(window, cx)),
            )
            .on_mouse_move(cx.listener(|this, e: &gpui::MouseMoveEvent, _, cx| {
                let Some(viewer) = this.chat.images.viewer.as_mut() else {
                    return;
                };
                if let Some(last) = viewer.drag {
                    if e.pressed_button != Some(MouseButton::Left) {
                        viewer.drag = None;
                        return;
                    }
                    viewer.pan += e.position - last;
                    viewer.drag = Some(e.position);
                    cx.notify();
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, _| {
                    if let Some(viewer) = this.chat.images.viewer.as_mut() {
                        viewer.drag = None;
                    }
                }),
            )
            // Orca: a pinch (ctrl + wheel) zooms; scrolling pans a zoomed picture.
            .on_scroll_wheel(cx.listener(|this, e: &gpui::ScrollWheelEvent, window, cx| {
                let Some(viewer) = this.chat.images.viewer.as_mut() else {
                    return;
                };
                let delta = e.delta.pixel_delta(window.line_height());
                if e.modifiers.control {
                    viewer.zoom_by((-f32::from(delta.y) / 150.).exp2());
                } else if !viewer.fitted() {
                    viewer.pan += delta;
                } else {
                    return;
                }
                cx.notify();
            }))
            .child(
                div()
                    .absolute()
                    .top(px(96.))
                    .bottom(px(96.))
                    .left(px(96.))
                    .right(px(96.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(picture),
            )
            .child(toolbar);
        Some(
            ui::modal_backdrop(viewport, ui::modal_focus_scope(surface, &focus)).into_any_element(),
        )
    }
}
