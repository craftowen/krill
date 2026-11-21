// Adapted from Orca (Copyright (c) 2026 Lovecast Inc., MIT License).
//! Orca's "New bot" dialog: a mascot carousel with background swatches and a picture upload,
//! a large name field, an optional job line, "Runs on … with … · Change", and Create (⌘↵).
//!
//! The host `Shell` draws it over the workspace and opens the new bot's Home Chat on
//! [`Event::Created`].

use std::sync::Arc;

use gpui::{
    App, ClickEvent, Context, Entity, EventEmitter, ExternalPaths, FocusHandle, FontWeight, Image,
    KeyDownEvent, SharedString, Subscription, Task, TextAlign, Window, div, prelude::*, px,
    relative,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_component::{Disableable as _, IconName, Sizable as _};
use krill_proto::bots::{
    BotConfig, BotRpcRequest, BotRpcResponse, BotTemplate, CreateBotParams, CreateBotV2Params,
};

use super::avatar::{self, PICKER_MASCOTS, PICTURE_MAX_BYTES, PictureError, prepare_picture};
use super::rpc::{BOT_RUNTIMES, BotRpcCall, call_checked, runtime_display_label, runtime_models};
use crate::i18n::catalog::bots as text;
use crate::ui;

/// Orca's job-line limit; the name follows krilld's byte cap.
const NAME_MAX_BYTES: usize = krill_proto::bots::BOT_NAME_MAX_BYTES;
const PURPOSE_MAX: usize = 200;

/// Carousel geometry (Orca `size-28`, neighbours scaled to 72 px, ±110 px from the centre).
const FACE: f32 = 112.;
const NEIGHBOUR: f32 = 72.;
const CAROUSEL_HEIGHT: f32 = 120.;
const DIALOG_WIDTH: f32 = 448.;
const TEMPLATE_DIALOG_WIDTH: f32 = 576.;

#[derive(Debug, Clone)]
pub enum Event {
    /// The bot exists. `picture_error` says why a picked picture couldn't be saved (the bot
    /// then keeps its default face).
    Created {
        bot: Box<BotConfig>,
        warning: Option<String>,
    },
    Close,
    /// "Start from a template": the host swaps this dialog for the templates gallery.
    BrowseTemplates,
}

/// One face in the carousel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Face {
    Mascot(&'static str),
    /// The uploaded picture.
    Custom,
}

/// First free "New bot", "New bot 2", … (Orca fills an empty name the same way).
#[must_use]
pub fn unique_name(base: &str, taken: &[String]) -> String {
    let is_taken = |n: &str| taken.iter().any(|t| t.trim().eq_ignore_ascii_case(n));
    if !is_taken(base) {
        return base.to_owned();
    }
    // One of the next `taken.len() + 1` numbers is always free.
    (2..=taken.len() + 2)
        .map(|i| format!("{base} {i}"))
        .find(|n| !is_taken(n))
        .unwrap_or_else(|| base.to_owned())
}

pub struct NewBot {
    focus: FocusHandle,
    carousel_focus: FocusHandle,
    rpc: BotRpcCall,
    host_label: SharedString,
    host_is_local: bool,
    taken_names: Vec<String>,
    faces: Vec<Face>,
    current: usize,
    background: u32,
    /// Prepared 512 px PNG and its decoded image.
    picture: Option<(Vec<u8>, Arc<Image>)>,
    picture_error: Option<PictureError>,
    name: Entity<InputState>,
    /// Single line: only single-line inputs centre their text like Orca's field.
    purpose: Entity<InputState>,
    runtime: &'static str,
    model: Option<&'static str>,
    changing: bool,
    template: Option<&'static BotTemplate>,
    /// Template skills the user kept checked.
    skills: Vec<&'static str>,
    submitting: bool,
    error: Option<SharedString>,
    task: Option<Task<()>>,
    picture_task: Option<Task<()>>,
    _subs: Vec<Subscription>,
}

impl EventEmitter<Event> for NewBot {}

impl std::fmt::Debug for NewBot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NewBot")
            .field("face", &self.faces.get(self.current))
            .field("runtime", &self.runtime)
            .field("submitting", &self.submitting)
            .finish_non_exhaustive()
    }
}

impl NewBot {
    /// `template` pre-fills the dialog the way Orca's "Use template…" does.
    pub fn new(
        rpc: BotRpcCall,
        host_label: SharedString,
        host_is_local: bool,
        taken_names: Vec<String>,
        template: Option<&'static BotTemplate>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let name = cx.new(|cx| {
            let mut s = InputState::new(window, cx).placeholder(text::NB_NAME_PLACEHOLDER.get());
            if let Some(t) = template {
                s.set_value(unique_name(t.name, &taken_names), window, cx);
            }
            s
        });
        let purpose = cx.new(|cx| {
            let mut s = InputState::new(window, cx).placeholder(text::NB_PURPOSE_PLACEHOLDER.get());
            if let Some(t) = template {
                s.set_value(t.description.to_owned(), window, cx);
            }
            s
        });
        let mut faces: Vec<Face> = PICKER_MASCOTS.iter().map(|m| Face::Mascot(m)).collect();
        let current = match template.and_then(|t| avatar::mascot_id(t.mascot)) {
            Some(id) => faces
                .iter()
                .position(|f| *f == Face::Mascot(id))
                .unwrap_or_else(|| {
                    // `S2` stays out of the picker unless a template asks for it.
                    faces.insert(0, Face::Mascot(id));
                    0
                }),
            None => random_index(faces.len()),
        };
        let subs = vec![
            cx.subscribe_in(&name, window, Self::on_input),
            cx.subscribe_in(&purpose, window, Self::on_input),
        ];
        Self {
            focus: cx.focus_handle(),
            carousel_focus: cx.focus_handle().tab_stop(true),
            rpc,
            host_label,
            host_is_local,
            taken_names,
            faces,
            current,
            background: avatar::DEFAULT_BACKGROUND,
            picture: None,
            picture_error: None,
            name,
            purpose,
            runtime: available_runtimes(host_is_local, cx)
                .first()
                .copied()
                .unwrap_or("claude"),
            model: None,
            changing: false,
            template,
            skills: template.map(|t| t.skills.to_vec()).unwrap_or_default(),
            submitting: false,
            error: None,
            task: None,
            picture_task: None,
            _subs: subs,
        }
    }

    #[must_use]
    pub fn focus_handle(&self) -> FocusHandle {
        self.focus.clone()
    }

    /// Puts the cursor in the name field (Orca focuses it when the dialog opens).
    pub fn focus_name(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.name.update(cx, |s, cx| s.focus(window, cx));
    }

    /// Pre-fills the job line ("Blank bot for “…”" from the templates search).
    pub fn set_purpose(&mut self, purpose: String, window: &mut Window, cx: &mut Context<Self>) {
        self.purpose
            .update(cx, |s, cx| s.set_value(purpose, window, cx));
    }

    fn on_input(
        &mut self,
        input: &Entity<InputState>,
        ev: &InputEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match ev {
            // Enter in either field submits, like Orca's form; ⌘↵ works from anywhere.
            InputEvent::PressEnter { .. }
                if !crate::jump::entity_input_is_composing(input, window, cx) =>
            {
                self.submit(cx);
            }
            // Orca's `maxLength`: the name keeps to krilld's byte cap, the job to 200 characters.
            InputEvent::Change if !crate::jump::entity_input_is_composing(input, window, cx) => {
                let value = input.read(cx).value().to_string();
                let capped = if *input == self.name {
                    clip_bytes(&value, NAME_MAX_BYTES)
                } else {
                    clip_chars(&value, PURPOSE_MAX)
                };
                if capped.len() < value.len() {
                    let capped = capped.to_owned();
                    input.update(cx, |s, cx| s.set_value(capped, window, cx));
                }
                cx.notify();
            }
            InputEvent::Change => cx.notify(),
            _ => {}
        }
    }

    fn input_is_composing(&self, window: &mut Window, cx: &mut App) -> bool {
        crate::jump::entity_input_is_composing(&self.name, window, cx)
            || crate::jump::entity_input_is_composing(&self.purpose, window, cx)
    }

    fn face(&self) -> Face {
        self.faces[self.current]
    }

    /// Steps the carousel, wrapping at both ends like Orca.
    pub fn step(&mut self, delta: isize, cx: &mut Context<Self>) {
        let len = self.faces.len().cast_signed();
        self.current = (self.current.cast_signed() + delta)
            .rem_euclid(len)
            .cast_unsigned();
        self.picture_error = None;
        cx.notify();
    }

    fn select_face(&mut self, index: usize, cx: &mut Context<Self>) {
        self.current = index;
        self.picture_error = None;
        cx.notify();
    }

    pub fn set_background(&mut self, rgb: u32, cx: &mut Context<Self>) {
        self.background = rgb;
        self.picture_error = None;
        cx.notify();
    }

    fn form_is_empty(&self, cx: &App) -> bool {
        self.template.is_none()
            && self.name.read(cx).value().trim().is_empty()
            && self.purpose.read(cx).value().trim().is_empty()
    }

    fn pick_picture(&mut self, cx: &mut Context<Self>) {
        let chosen = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some(text::NB_UPLOAD.get().into()),
        });
        self.picture_task = Some(cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = chosen.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            this.update(cx, |this, cx| this.load_picture_from_path(path, cx))
                .ok();
        }));
    }

    fn load_picture_from_path(&mut self, path: std::path::PathBuf, cx: &mut Context<Self>) {
        self.load_picture(
            move || {
                // Refuse a huge file before reading it into memory.
                let len = std::fs::metadata(&path)
                    .map_err(|_| PictureError::Unreadable)?
                    .len();
                if usize::try_from(len).map_or(true, |n| n > PICTURE_MAX_BYTES) {
                    return Err(PictureError::TooBig);
                }
                std::fs::read(path).map_err(|_| PictureError::Unreadable)
            },
            cx,
        );
    }

    /// Decodes off the UI thread, then shows the picture as the selected face.
    pub fn load_picture(
        &mut self,
        read: impl FnOnce() -> Result<Vec<u8>, PictureError> + Send + 'static,
        cx: &mut Context<Self>,
    ) {
        self.picture_task = Some(cx.spawn(async move |this, cx| {
            let prepared = cx
                .background_executor()
                .spawn(async move { prepare_picture(&read()?) })
                .await;
            this.update(cx, |this, cx| {
                match prepared {
                    Ok(png) => {
                        let image =
                            Arc::new(Image::from_bytes(gpui::ImageFormat::Png, png.clone()));
                        this.picture = Some((png, image));
                        this.picture_error = None;
                        // Orca slots the picture right after the mascot being looked at.
                        if let Some(at) = this.faces.iter().position(|f| *f == Face::Custom) {
                            this.current = at;
                        } else {
                            let at = this.current + 1;
                            this.faces.insert(at, Face::Custom);
                            this.current = at;
                        }
                    }
                    Err(err) => this.picture_error = Some(err),
                }
                cx.notify();
            })
            .ok();
        }));
    }

    /// Pasting an image anywhere in the dialog uses it; text paste falls through to the field.
    fn paste_picture(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(item) = cx.read_from_clipboard() else {
            return false;
        };
        let Some(bytes) = item.entries().iter().find_map(|e| match e {
            gpui::ClipboardEntry::Image(img) if !img.bytes.is_empty() => Some(img.bytes.clone()),
            _ => None,
        }) else {
            return false;
        };
        let _ = window;
        self.load_picture(move || Ok(bytes), cx);
        true
    }

    fn set_runtime(&mut self, runtime: &'static str, cx: &mut Context<Self>) {
        if self.runtime != runtime {
            self.runtime = runtime;
            self.model = None;
        }
        cx.notify();
    }

    fn set_model(&mut self, model: Option<&'static str>, cx: &mut Context<Self>) {
        self.model = model;
        cx.notify();
    }

    pub(super) fn toggle_skill(&mut self, skill: &'static str, cx: &mut Context<Self>) {
        if let Some(i) = self.skills.iter().position(|s| *s == skill) {
            self.skills.remove(i);
        } else {
            self.skills.push(skill);
        }
        cx.notify();
    }

    /// Builds the create request from what the dialog shows.
    fn params(&self, cx: &App) -> CreateBotV2Params {
        let typed = clip_bytes(self.name.read(cx).value().trim(), NAME_MAX_BYTES).to_owned();
        let name = if typed.is_empty() {
            unique_name(text::NB_DEFAULT_NAME.get(), &self.taken_names)
        } else {
            typed
        };
        let description = clip_chars(self.purpose.read(cx).value().trim(), PURPOSE_MAX).to_owned();
        let (avatar, avatar_color) = match self.face() {
            Face::Mascot(id) => (
                Some(id.to_owned()),
                (self.background != avatar::DEFAULT_BACKGROUND)
                    .then(|| avatar::format_color(self.background)),
            ),
            // The picture is uploaded right after the bot exists.
            Face::Custom => (None, None),
        };
        let template = self.template;
        CreateBotV2Params {
            base: CreateBotParams {
                name,
                agent_id: self.runtime.to_owned(),
                description,
                emoji: None,
                avatar,
                model: self.model.map(str::to_owned),
                effort: None,
                permission_mode: None,
                workspace: None,
                template_id: template.map(|t| t.id.to_owned()),
                initial_soul: template.map(|t| t.default_soul.to_owned()),
            },
            slug: None,
            avatar_color,
            skills: self.skills.iter().map(|s| (*s).to_owned()).collect(),
            // krilld adds the template's event rules (turned off) itself.
            events: Vec::new(),
        }
    }

    pub fn submit(&mut self, cx: &mut Context<Self>) {
        if self.submitting {
            return;
        }
        let params = self.params(cx);
        let picture = (self.face() == Face::Custom)
            .then(|| self.picture.as_ref().map(|(png, _)| png.clone()))
            .flatten();
        // krilld fills in a template's skills when none are sent, so "all unchecked" is
        // applied right after the bot exists.
        let clear_skills =
            self.template.is_some_and(|t| !t.skills.is_empty()) && self.skills.is_empty();
        let rpc = self.rpc.clone();
        self.submitting = true;
        self.error = None;
        cx.notify();
        self.task = Some(cx.spawn(async move |this, cx| {
            let created = cx
                .background_executor()
                .spawn(async move {
                    let req = BotRpcRequest::CreateBotV2(Box::new(params));
                    let mut bot = match call_checked(&rpc, &req)? {
                        BotRpcResponse::Bot(bot) => bot,
                        other => return Err(format!("unexpected CreateBot response: {other:?}")),
                    };
                    // What didn't stick after the bot was made (shown as a notice).
                    let mut warning = None;
                    if clear_skills {
                        let req = BotRpcRequest::UpdateBotExtras(Box::new(
                            krill_proto::bots::UpdateBotExtrasParams {
                                bot_id: bot.id.clone(),
                                avatar_color: krill_proto::bots::FieldPatch::Unchanged,
                                skills: Some(Vec::new()),
                                events: None,
                            },
                        ));
                        match call_checked(&rpc, &req) {
                            Ok(BotRpcResponse::Bot(updated)) => bot = updated,
                            Ok(_) => bot.skills.clear(),
                            Err(err) => warning = Some(err),
                        }
                    }
                    if let Some(png_bytes) = picture {
                        // The bot exists either way; say why its picture didn't stick.
                        let upload = BotRpcRequest::UploadBotAvatar {
                            bot_id: bot.id.clone(),
                            png_bytes,
                        };
                        match call_checked(&rpc, &upload) {
                            Ok(BotRpcResponse::BotAvatar(_)) => {
                                bot.avatar = Some(avatar::UPLOAD_AVATAR.to_owned());
                            }
                            Ok(other) => {
                                warning =
                                    Some(format!("unexpected UploadBotAvatar response: {other:?}"));
                            }
                            Err(err) => warning = Some(err),
                        }
                    }
                    Ok::<_, String>((bot, warning))
                })
                .await;
            this.update(cx, |this, cx| {
                this.submitting = false;
                match created {
                    Ok((bot, warning)) => cx.emit(Event::Created { bot, warning }),
                    Err(err) => this.error = Some(SharedString::from(err)),
                }
                cx.notify();
            })
            .ok();
        }));
    }

    fn close(&mut self, cx: &mut Context<Self>) {
        // Orca ignores closing while the bot is being made.
        if !self.submitting {
            cx.emit(Event::Close);
        }
    }

    fn on_key(&mut self, e: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.input_is_composing(window, cx) {
            return;
        }
        let keys = &e.keystroke;
        match keys.key.as_str() {
            "escape" if !keys.modifiers.modified() => {
                cx.stop_propagation();
                self.close(cx);
            }
            "enter"
                if keys.modifiers.secondary() && !keys.modifiers.shift && !keys.modifiers.alt =>
            {
                cx.stop_propagation();
                self.submit(cx);
            }
            "left" | "right" if self.carousel_focus.is_focused(window) => {
                cx.stop_propagation();
                self.step(if keys.key == "left" { -1 } else { 1 }, cx);
            }
            _ => {}
        }
    }
}

/// The longest prefix of `s` within `max` bytes that ends on a character boundary.
fn clip_bytes(s: &str, max: usize) -> &str {
    let mut end = s.len().min(max);
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    s.get(..end).unwrap_or(s)
}

/// The first `max` characters of `s`.
fn clip_chars(s: &str, max: usize) -> &str {
    s.char_indices()
        .nth(max)
        .and_then(|(i, _)| s.get(..i))
        .unwrap_or(s)
}

/// Runtimes a new bot can start with: on this computer, the installed ones (all of them
/// before detection finishes, or for a remote host).
fn available_runtimes(local: bool, cx: &App) -> Vec<&'static str> {
    BOT_RUNTIMES
        .iter()
        .map(|rt| rt.id)
        .filter(|id| !local || crate::accounts::installed::agent_maybe_installed(cx, id))
        .collect()
}

fn random_index(len: usize) -> usize {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos());
    usize::try_from(nanos).unwrap_or(0) % len.max(1)
}

impl NewBot {
    fn face_image(&self, face: Face) -> Option<Arc<Image>> {
        match face {
            Face::Mascot(id) => avatar::mascot_image(id, self.background),
            Face::Custom => self.picture.as_ref().map(|(_, img)| img.clone()),
        }
    }

    fn render_face(&self, index: usize, size: f32, cx: &mut Context<Self>) -> gpui::AnyElement {
        let current = index == self.current;
        let image = self.face_image(self.faces[index]);
        let id = SharedString::from(format!("new-bot-face-{index}"));
        div()
            .id(id.clone())
            .debug_selector(move || id.to_string())
            .flex_none()
            .size(px(size))
            .when(!current, |d| {
                d.opacity(0.4)
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, _, cx| this.select_face(index, cx)))
            })
            .children(image.map(|img| {
                gpui::img(img)
                    .size(px(size))
                    .rounded(if current {
                        ui::RADIUS_XL
                    } else {
                        ui::RADIUS_LG
                    })
                    .object_fit(gpui::ObjectFit::Cover)
            }))
            .into_any_element()
    }

    fn chevron(
        id: &'static str,
        icon: IconName,
        tip: &'static str,
        delta: isize,
        cx: &mut Context<Self>,
    ) -> Button {
        ui::button_with_tooltip(
            Button::new(id)
                .ghost()
                .small()
                .icon(icon)
                .debug_selector(move || id.into())
                .on_click(cx.listener(move |this, _, _, cx| this.step(delta, cx))),
            id,
            tip,
        )
    }

    fn render_carousel(
        &self,
        p: &ui::Palette,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let len = self.faces.len();
        let prev = (self.current + len - 1) % len;
        let next = (self.current + 1) % len;
        let focused = self.carousel_focus.is_focused(window);
        let live = match self.face() {
            Face::Custom => text::NB_YOUR_IMAGE.get().to_owned(),
            Face::Mascot(id) => text::NB_MASCOT_N.format(&[
                (
                    "n",
                    &(PICKER_MASCOTS.iter().position(|m| *m == id).unwrap_or(0) + 1).to_string(),
                ),
                ("total", &PICKER_MASCOTS.len().to_string()),
            ]),
        };
        let prev_button = Self::chevron(
            "new-bot-prev",
            IconName::ChevronLeft,
            text::NB_PREV_PICTURE.get(),
            -1,
            cx,
        );
        let next_button = Self::chevron(
            "new-bot-next",
            IconName::ChevronRight,
            text::NB_NEXT_PICTURE.get(),
            1,
            cx,
        );
        div()
            .id("new-bot-carousel")
            .debug_selector(|| "new-bot-carousel".into())
            .track_focus(&self.carousel_focus)
            .role(gpui::Role::Group)
            .aria_label(text::NB_PICTURE.get())
            .relative()
            .w_full()
            .h(px(CAROUSEL_HEIGHT))
            .flex()
            .items_center()
            .justify_center()
            .gap(px((2.0f32.mul_add(110., -FACE) - NEIGHBOUR) / 2.))
            .child(self.render_face(prev, NEIGHBOUR, cx))
            .child(
                div()
                    .rounded(ui::RADIUS_XL)
                    .border_2()
                    .border_color(if focused { p.ring } else { ui::TRANSPARENT })
                    .child(self.render_face(self.current, FACE, cx)),
            )
            .child(self.render_face(next, NEIGHBOUR, cx))
            .child(
                div()
                    .absolute()
                    .left_2()
                    .top_0()
                    .bottom_0()
                    .flex()
                    .items_center()
                    .child(prev_button),
            )
            .child(
                div()
                    .absolute()
                    .right_2()
                    .top_0()
                    .bottom_0()
                    .flex()
                    .items_center()
                    .child(next_button),
            )
            .child(
                div()
                    .id("new-bot-carousel-live")
                    .absolute()
                    .size_0()
                    .overflow_hidden()
                    .aria_label(live),
            )
    }

    fn render_swatches(
        &self,
        p: &ui::Palette,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let mut row = div()
            .id("new-bot-swatches")
            .role(gpui::Role::Group)
            .aria_label(text::NB_BACKGROUND.get())
            .mt_2()
            .flex()
            .flex_wrap()
            .items_center()
            .justify_center()
            .gap_1();
        for (rgb, name) in ui::BOT_AVATAR_BACKGROUNDS {
            let selected = self.background == rgb;
            let id = SharedString::from(format!("new-bot-swatch-{rgb:06x}"));
            row = row.child(
                div()
                    .id(id.clone())
                    .debug_selector(move || id.to_string())
                    .role(gpui::Role::Button)
                    .aria_label(name)
                    .tab_index(0)
                    .focus_visible(|d| d.border_color(p.ring))
                    .flex_none()
                    .size(px(20.))
                    .rounded_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .when(selected, |d| d.border_2().border_color(p.foreground))
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, _, cx| this.set_background(rgb, cx)))
                    .child(
                        div()
                            .size(px(16.))
                            .rounded_full()
                            .bg(ui::bot_avatar_background(rgb))
                            .border_1()
                            .border_color(p.border),
                    ),
            );
        }
        row
    }

    fn render_upload_line(p: &ui::Palette, cx: &mut Context<Self>) -> gpui::Div {
        div()
            .mt_0p5()
            .h(px(28.))
            .flex()
            .items_center()
            .justify_center()
            .text_size(ui::TEXT_SM)
            .text_color(p.muted_foreground)
            .child(
                div()
                    .id("new-bot-upload")
                    .debug_selector(|| "new-bot-upload".into())
                    .role(gpui::Role::Button)
                    .tab_index(0)
                    .focus_visible(|d| d.border_1().border_color(p.ring))
                    .cursor_pointer()
                    .underline()
                    .hover(|d| d.text_color(p.foreground))
                    .on_click(cx.listener(|this, _, _, cx| this.pick_picture(cx)))
                    .child(text::NB_UPLOAD.get()),
            )
            .child(text::NB_UPLOAD_REST.get())
    }

    fn render_runs_on(&self, p: &ui::Palette, cx: &mut Context<Self>) -> gpui::Stateful<gpui::Div> {
        let strong = |s: SharedString| {
            div()
                .font_weight(FontWeight::MEDIUM)
                .text_color(p.foreground)
                .child(s)
        };
        let mut agent = runtime_display_label(self.runtime);
        if let Some(model) = self.model {
            let label = runtime_models(self.runtime)
                .iter()
                .find(|(id, _)| *id == model)
                .map_or(model, |(_, l)| *l);
            agent = format!("{agent} ({label})");
        }
        // "Runs on {host} with {agent}": icons sit next to the names, word order per language.
        let line = text::NB_RUNS_LINE.get();
        let (before_host, rest) = line.split_once("{host}").unwrap_or((line, ""));
        let (between, after_agent) = rest.split_once("{agent}").unwrap_or((rest, ""));
        div()
            .id("new-bot-runs-on")
            .debug_selector(|| "new-bot-runs-on".into())
            .role(gpui::Role::Button)
            .tab_index(0)
            .focus_visible(|d| d.border_1().border_color(p.ring))
            .mx_auto()
            .px_2()
            .py_1()
            .rounded(ui::RADIUS_SM)
            .cursor_pointer()
            .hover(|d| d.bg(p.accent.opacity(0.5)))
            .flex()
            .flex_wrap()
            .items_center()
            .justify_center()
            .gap_x_1()
            .text_size(ui::TEXT_SM)
            .text_color(p.muted_foreground)
            .on_click(cx.listener(|this, _, _, cx| {
                this.changing = !this.changing;
                cx.notify();
            }))
            .when(!before_host.trim().is_empty(), |d| {
                d.child(before_host.trim().to_owned())
            })
            .child(ui::icons::svg_icon(
                if self.host_is_local {
                    ui::icons::MONITOR_SVG
                } else {
                    ui::icons::CLOUD_SVG
                },
                ui::ICON_XS,
                p.muted_foreground,
            ))
            .child(strong(self.host_label.clone()))
            .when(!between.trim().is_empty(), |d| {
                d.child(between.trim().to_owned())
            })
            .child(ui::icons::agent_brand_icon(
                self.runtime,
                ui::ICON_XS,
                p.foreground,
            ))
            .child(strong(agent.into()))
            .when(!after_agent.trim().is_empty(), |d| {
                d.child(after_agent.trim().to_owned())
            })
            .child("·")
            .child(div().underline().child(text::NB_CHANGE.get()))
    }

    fn render_change(&self, p: &ui::Palette, cx: &mut Context<Self>) -> gpui::Stateful<gpui::Div> {
        let this = cx.weak_entity();
        let runtime = self.runtime;
        let runtime_button = Button::new("new-bot-runtime")
            .outline()
            .small()
            .w_full()
            .dropdown_caret(true)
            .accessibility_label(runtime_display_label(runtime))
            .debug_selector(|| "new-bot-runtime".into())
            .child(
                div()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .child(ui::icons::agent_brand_icon(
                        runtime,
                        ui::ICON_SM,
                        p.foreground,
                    ))
                    .child(div().truncate().child(runtime_display_label(runtime))),
            )
            .dropdown_menu(move |mut menu, _, cx| {
                // Orca lists only agents this computer can run (all of them for a remote host).
                let local = this.upgrade().is_some_and(|v| v.read(cx).host_is_local);
                for rt in BOT_RUNTIMES.iter().filter(|rt| {
                    !local || crate::accounts::installed::agent_maybe_installed(cx, rt.id)
                }) {
                    let this = this.clone();
                    menu = menu.item(
                        PopupMenuItem::new(rt.label())
                            .icon(ui::icons::agent_menu_icon(rt.id))
                            .checked(runtime == rt.id)
                            .on_click(move |_, _, cx| {
                                this.update(cx, |v, cx| v.set_runtime(rt.id, cx)).ok();
                            }),
                    );
                }
                menu
            });
        let this = cx.weak_entity();
        let current = self.model;
        let presets = runtime_models(runtime);
        let model_label = current
            .and_then(|m| presets.iter().find(|(id, _)| *id == m).map(|(_, l)| *l))
            .unwrap_or(text::MODEL_DEFAULT.get());
        let model_button = Button::new("new-bot-model")
            .outline()
            .small()
            .w_full()
            .dropdown_caret(true)
            .label(model_label)
            .debug_selector(|| "new-bot-model".into())
            .dropdown_menu(move |mut menu, _, _| {
                let pick = |choice: Option<&'static str>| {
                    let this = this.clone();
                    move |_: &ClickEvent, _: &mut Window, cx: &mut App| {
                        this.update(cx, |v, cx| v.set_model(choice, cx)).ok();
                    }
                };
                menu = menu.item(
                    PopupMenuItem::new(text::MODEL_DEFAULT.get())
                        .checked(current.is_none())
                        .on_click(pick(None)),
                );
                for (id, label) in presets {
                    menu = menu.item(
                        PopupMenuItem::new(*label)
                            .checked(current == Some(*id))
                            .on_click(pick(Some(*id))),
                    );
                }
                menu
            });
        let field = |label: &'static str| {
            div()
                .text_size(ui::TEXT_SM)
                .font_weight(FontWeight::MEDIUM)
                .text_color(p.foreground)
                .child(label)
        };
        div()
            .id("new-bot-change")
            .debug_selector(|| "new-bot-change".into())
            .mt_3()
            .pt_3()
            .border_t_1()
            .border_color(p.border)
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(field(text::NB_COMPUTER.get()))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1p5()
                            .text_size(ui::TEXT_LG)
                            .child(ui::icons::svg_icon(
                                if self.host_is_local {
                                    ui::icons::MONITOR_SVG
                                } else {
                                    ui::icons::CLOUD_SVG
                                },
                                ui::ICON_SM,
                                p.muted_foreground,
                            ))
                            .child(self.host_label.clone()),
                    )
                    .child(
                        div()
                            .text_size(ui::TEXT_XS)
                            .text_color(p.muted_foreground)
                            .child(text::NB_COMPUTER_HINT.get()),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(field(text::NB_AGENT.get()))
                    .child(
                        div()
                            .flex()
                            .gap_2()
                            .child(div().flex_1().min_w_0().child(runtime_button))
                            .child(div().flex_1().min_w_0().child(model_button)),
                    ),
            )
    }

    fn render_comes_with(
        &self,
        tpl: &'static BotTemplate,
        p: &ui::Palette,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let list = super::templates::comes_with_rows(tpl, p, |skill| {
            let id = SharedString::from(format!("new-bot-skill-{skill}"));
            Some(
                gpui_component::checkbox::Checkbox::new(id)
                    .checked(self.skills.contains(&skill))
                    .accessibility_label(text::NB_ADD_SKILL.format(&[("skill", skill)]))
                    .on_click(cx.listener(move |this, _, _, cx| this.toggle_skill(skill, cx)))
                    .into_any_element(),
            )
        });
        div()
            .pt_4()
            .border_t_1()
            .border_color(p.border)
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .text_size(ui::TEXT_SM)
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(p.foreground)
                    .child(text::NB_COMES_WITH.get()),
            )
            .child(
                div()
                    .text_size(ui::TEXT_XS)
                    .text_color(p.muted_foreground)
                    .child(text::NB_COMES_WITH_TIP.get()),
            )
            .children(list)
    }

    fn render_footer(&self, p: &ui::Palette, cx: &mut Context<Self>) -> gpui::Div {
        let show_templates = self.form_is_empty(cx);
        let chord = if cfg!(target_os = "macos") {
            "⌘ ↵"
        } else {
            "Ctrl ↵"
        };
        div()
            .pt_1()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_3()
            .when(show_templates, |d| {
                d.child(
                    div()
                        .id("new-bot-start-from-template")
                        .debug_selector(|| "new-bot-start-from-template".into())
                        .role(gpui::Role::Button)
                        .tab_index(0)
                        .focus_visible(|d| d.border_1().border_color(p.ring))
                        .cursor_pointer()
                        .flex()
                        .items_center()
                        .gap_1p5()
                        .text_size(ui::TEXT_SM)
                        .text_color(p.muted_foreground)
                        .hover(|d| d.text_color(p.foreground))
                        .on_click(cx.listener(|_, _, _, cx| cx.emit(Event::BrowseTemplates)))
                        .child(ui::icons::svg_icon(
                            ui::icons::LAYOUT_TEMPLATE_SVG,
                            ui::ICON_MD,
                            p.muted_foreground,
                        ))
                        .child(text::NB_START_FROM_TEMPLATE.get()),
                )
            })
            .child(
                div().ml_auto().child(
                    ui::primary("new-bot-create", text::NB_CREATE.get())
                        .debug_selector(|| "new-bot-create".into())
                        .disabled(
                            self.submitting
                                || available_runtimes(self.host_is_local, cx).is_empty(),
                        )
                        .on_click(cx.listener(|this, _, _, cx| this.submit(cx)))
                        .child(
                            div()
                                .ml_1()
                                .px_1p5()
                                .py_0p5()
                                .rounded(ui::RADIUS_XS)
                                .border_1()
                                .border_color(p.primary_foreground.opacity(0.2))
                                .text_size(ui::TEXT_2XS)
                                .font_weight(FontWeight::MEDIUM)
                                .child(chord),
                        ),
                ),
            )
    }
}

impl Render for NewBot {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = ui::palette(cx);
        let viewport = window.viewport_size();
        let width = if self.template.is_some() {
            TEMPLATE_DIALOG_WIDTH
        } else {
            DIALOG_WIDTH
        };
        let subtitle: SharedString = match self.template {
            Some(t) => text::NB_FROM_TEMPLATE
                .format(&[("template", t.name)])
                .into(),
            None => text::NB_SUBTITLE.get().into(),
        };
        let custom = self.face() == Face::Custom;
        let purpose_len = self.purpose.read(cx).value().chars().count();

        let picker = div()
            .id("new-bot-picker")
            .relative()
            .rounded(ui::RADIUS_XL)
            .border_1()
            .border_color(ui::TRANSPARENT)
            .drag_over::<ExternalPaths>(move |d, _, _, _| {
                d.border_dashed().border_color(p.foreground.opacity(0.35))
            })
            .child(self.render_carousel(p, window, cx))
            // Orca hides the swatches for a picture; keep their space so nothing below moves.
            .child(self.render_swatches(p, cx).when(custom, Styled::invisible))
            .child(match self.picture_error {
                Some(err) => div()
                    .id("new-bot-picture-error")
                    .role(gpui::Role::Alert)
                    .mt_0p5()
                    .h(px(28.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .gap_1()
                    .text_size(ui::TEXT_SM)
                    .text_color(p.destructive)
                    .child(ui::icons::svg_icon(
                        ui::icons::TRIANGLE_ALERT_SVG,
                        ui::ICON_MD,
                        p.destructive,
                    ))
                    .child(err.message())
                    .into_any_element(),
                None => Self::render_upload_line(p, cx).into_any_element(),
            });

        let fields = div()
            .flex()
            .flex_col()
            .gap_1()
            .child(
                Input::new(&self.name)
                    .appearance(false)
                    .text_align(TextAlign::Center)
                    .text_size(ui::TEXT_2XL)
                    .font_weight(FontWeight::SEMIBOLD)
                    .px_9(),
            )
            .child(
                Input::new(&self.purpose)
                    .appearance(false)
                    .text_align(TextAlign::Center)
                    .text_size(ui::TEXT_LG)
                    .text_color(p.muted_foreground)
                    .px_9(),
            )
            // The counter's line is always there; it only becomes visible near the limit.
            .child(
                div()
                    .text_center()
                    .text_size(ui::TEXT_SM)
                    .text_color(p.muted_foreground)
                    .when(purpose_len < 160, Styled::invisible)
                    .child(format!("{purpose_len}/{PURPOSE_MAX}")),
            );

        let body = div()
            .id("new-bot-body")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap_4()
            .child(picker)
            .child(fields)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .child(self.render_runs_on(p, cx))
                    .when(
                        !self.changing && available_runtimes(self.host_is_local, cx).is_empty(),
                        |d| {
                            d.child(
                                div()
                                    .mt_1()
                                    .text_center()
                                    .text_size(ui::TEXT_XS)
                                    .text_color(p.muted_foreground)
                                    .child(text::NB_NO_AGENTS.get()),
                            )
                        },
                    )
                    .when(self.changing, |d| d.child(self.render_change(p, cx))),
            )
            .children(self.template.map(|t| self.render_comes_with(t, p, cx)))
            .children(self.error.clone().map(|e| ui::error_banner(p, e)));

        let card = div()
            .id("new-bot-dialog")
            .debug_selector(|| "new-bot-dialog".into())
            .role(gpui::Role::Dialog)
            .aria_label(text::NB_TITLE.get())
            .occlude()
            .relative()
            .w(px(width))
            .max_w((viewport.width - ui::SPACE_4 * 2.).max(px(0.)))
            .max_h((viewport.height - ui::SPACE_4 * 2.).max(px(0.)))
            .p_6()
            .rounded(ui::RADIUS_LG)
            .bg(p.popover)
            .text_color(p.popover_foreground)
            .font_family(ui::FONT_UI)
            .text_size(ui::TEXT_MD)
            .border_1()
            .border_color(p.border)
            .shadow_lg()
            .flex()
            .flex_col()
            .gap_4()
            .capture_action(
                cx.listener(|this, _: &gpui_component::input::Paste, window, cx| {
                    if this.paste_picture(window, cx) {
                        cx.stop_propagation();
                    }
                }),
            )
            .capture_action(
                cx.listener(|this, _: &gpui_component::input::Escape, window, cx| {
                    if this.input_is_composing(window, cx) {
                        window.prevent_default();
                    }
                }),
            )
            .on_key_down(
                cx.listener(|this, e: &KeyDownEvent, window, cx| this.on_key(e, window, cx)),
            )
            .on_drop(cx.listener(|this, paths: &ExternalPaths, _, cx| {
                if let Some(path) = paths.paths().first().cloned() {
                    this.load_picture_from_path(path, cx);
                }
            }))
            // A click on the card (or a menu it opened) stays here; the backdrop closes.
            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                div().absolute().top_4().right_4().child(
                    Button::new("new-bot-close")
                        .ghost()
                        .xsmall()
                        .icon(IconName::Close)
                        .accessibility_label(text::CLOSE.get())
                        .debug_selector(|| "new-bot-close".into())
                        .on_click(cx.listener(|this, _, _, cx| this.close(cx))),
                ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap_2()
                    .text_center()
                    .child(
                        div()
                            .text_size(ui::TEXT_XL)
                            .font_weight(FontWeight::SEMIBOLD)
                            .line_height(relative(1.375))
                            .child(text::NB_TITLE.get()),
                    )
                    .child(
                        div()
                            .text_size(ui::TEXT_LG)
                            .text_color(p.muted_foreground)
                            .child(subtitle),
                    ),
            )
            .child(body)
            .child(self.render_footer(p, cx));
        ui::modal_backdrop(
            viewport,
            div()
                .id("new-bot-backdrop")
                .debug_selector(|| "new-bot-backdrop".into())
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .on_mouse_down(
                    gpui::MouseButton::Left,
                    cx.listener(|this, _, _, cx| this.close(cx)),
                )
                .child(ui::modal_focus_scope(card, &self.focus)),
        )
    }
}

impl gpui::Focusable for NewBot {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_keep_to_krillds_byte_cap_on_character_boundaries() {
        let korean = "가".repeat(30);
        let clipped = clip_bytes(&korean, NAME_MAX_BYTES);
        assert_eq!(clipped.chars().count(), 21);
        assert!(clipped.len() <= NAME_MAX_BYTES);
        assert_eq!(clip_chars("abcdef", 3), "abc");
        assert_eq!(clip_chars("ab", 3), "ab");
    }

    #[test]
    fn empty_names_fill_in_like_orca() {
        assert_eq!(unique_name("New bot", &[]), "New bot");
        let taken = vec!["new bot".to_owned(), "New bot 2".to_owned()];
        assert_eq!(unique_name("New bot", &taken), "New bot 3");
    }
}
