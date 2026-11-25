// Adapted from Orca (Copyright (c) 2026 Lovecast Inc., MIT License).
//! Shared bot picture state & Orca `BotPicturePicker` + `AvatarGenerateFace` UI for `NewBot` and `ImportBotDialog`.

use std::path::PathBuf;
use std::sync::Arc;

use gpui::{
    AnyElement, App, Context, Entity, ExternalPaths, FocusHandle, Image, KeyDownEvent, ObjectFit,
    SharedString, Subscription, Task, Window, div, prelude::*, px,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::{Input, InputEvent, InputState, Textarea, TextareaState};
use gpui_component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_component::popover::Popover;
use gpui_component::{Disableable as _, Icon, IconName, Sizable as _};
use gpui_kit_assets::IconName as KitIconName;

use super::pickers::PopoverTrigger;

use super::avatar::{
    self, MASCOT_IDS, PICKER_MASCOTS, PICTURE_MAX_BYTES, PictureError, prepare_picture,
};
use super::avatar_gen::{
    self, AvatarCancelHandle, AvatarGenerationCapability, AvatarGenerationFailure,
    AvatarImageAgent, AvatarRunResult, MAX_AVATAR_DESCRIPTION_CHARS, pick_avatar_agent,
};
use crate::i18n::catalog::bots as text;
use crate::ui;

const PICKER_FACE: f32 = 112.;
const PICKER_NEIGHBOUR: f32 = 72.;
const PICKER_CAROUSEL_HEIGHT: f32 = 120.;

/// Which face is selected in the picture picker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PictureFace {
    Mascot(&'static str),
    Custom,
    Generate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GenerateAnchor {
    Custom,
    Mascot(usize),
}

type StoredPicture = (Vec<u8>, Arc<Image>, Option<u32>, String);

#[derive(Debug, Clone)]
pub enum IntakeLine {
    Hint,
    Saved {
        prev_custom: Option<StoredPicture>,
        prev_was_custom: bool,
        prev_mascot: usize,
        prev_bg: u32,
    },
    Error(PictureError),
}

#[derive(Debug, Clone)]
pub enum GenRunState {
    Idle,
    Running {
        agent: AvatarImageAgent,
        cancel: AvatarCancelHandle,
    },
    Error {
        failure: AvatarGenerationFailure,
        agent: Option<AvatarImageAgent>,
    },
}

/// Reusable state for `BotPicturePicker` (`NewBot` and `ImportBotDialog`).
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent UI state flags for PictureState controls"
)]
pub struct PictureState {
    pub prefix: &'static str,
    pub carousel_focus: FocusHandle,
    pub mascots: Vec<&'static str>,
    /// Index into `mascots` currently selected (or nearest before `Custom`).
    pub mascot_idx: usize,
    pub showing_custom: bool,
    pub background: u32,
    /// Prepared 512 px PNG bytes, decoded GPUI image, outer-ring band tint, and label/prompt.
    pub custom: Option<StoredPicture>,
    pub custom_after: usize,
    pub popover_open: bool,
    pub on_generate: bool,
    generate_anchor: GenerateAnchor,
    pub intake: IntakeLine,
    intake_seq: u64,
    reading: bool,
    picture_task: Option<Task<()>>,

    // Custom hex input
    pub hex_open: bool,
    hex_input: Option<Entity<InputState>>,
    hex_sub: Option<Subscription>,

    // Avatar generation (`AvatarGenerateFace`)
    pub capability: Option<AvatarGenerationCapability>,
    cap_task: Option<Task<()>>,
    pub gen_agent: Option<AvatarImageAgent>,
    pub gen_run: GenRunState,
    pub gen_made: Option<StoredPicture>,
    pub gen_editing: bool,
    gen_input: Option<Entity<TextareaState>>,
    gen_sub: Option<Subscription>,
    gen_task: Option<Task<()>>,
    gen_seq: u64,
}

impl Drop for PictureState {
    fn drop(&mut self) {
        self.cancel_running_generation();
        // Orca #47278: release uploaded/generated PNG buffers and decoded GPUI images promptly.
        self.custom = None;
        self.gen_made = None;
        self.intake = IntakeLine::Hint;
    }
}

impl PictureState {
    #[must_use]
    pub fn new(prefix: &'static str, template_mascot: Option<&'static str>, cx: &mut App) -> Self {
        let mut mascots: Vec<&'static str> = PICKER_MASCOTS.to_vec();
        let mascot_idx = if let Some(m) = template_mascot {
            if let Some(pos) = mascots.iter().position(|id| *id == m) {
                pos
            } else if MASCOT_IDS.contains(&m) {
                mascots.push(m);
                mascots.len() - 1
            } else {
                random_index(mascots.len())
            }
        } else {
            random_index(mascots.len())
        };

        let cached_cap = avatar_gen::cached_capability();
        let preferred_agent = crate::settings::current(cx)
            .avatar_generation_agent
            .as_deref()
            .and_then(AvatarImageAgent::from_id);
        let gen_agent = cached_cap
            .as_ref()
            .and_then(|c| pick_avatar_agent(&c.agents, preferred_agent));

        Self {
            prefix,
            carousel_focus: cx.focus_handle(),
            mascots,
            mascot_idx,
            showing_custom: false,
            background: avatar::DEFAULT_BACKGROUND,
            custom: None,
            custom_after: mascot_idx,
            popover_open: false,
            on_generate: false,
            generate_anchor: GenerateAnchor::Mascot(mascot_idx),
            intake: IntakeLine::Hint,
            intake_seq: 0,
            reading: false,
            picture_task: None,
            hex_open: false,
            hex_input: None,
            hex_sub: None,
            capability: cached_cap,
            cap_task: None,
            gen_agent,
            gen_run: GenRunState::Idle,
            gen_made: None,
            gen_editing: false,
            gen_input: None,
            gen_sub: None,
            gen_task: None,
            gen_seq: 0,
        }
    }

    pub fn refresh_capability<E: 'static>(
        &mut self,
        lens: fn(&mut E) -> &mut Self,
        cx: &mut Context<E>,
    ) {
        self.cap_task = Some(cx.spawn(async move |this, cx| {
            let cap = cx
                .background_executor()
                .spawn(async move { avatar_gen::probe_capability() })
                .await;
            this.update(cx, |this, cx| {
                let pref = crate::settings::current(cx)
                    .avatar_generation_agent
                    .as_deref()
                    .and_then(AvatarImageAgent::from_id);
                let state = lens(this);
                let chosen = state.gen_agent.or(pref);
                state.gen_agent = pick_avatar_agent(&cap.agents, chosen);
                state.capability = Some(cap);
                cx.notify();
            })
            .ok();
        }));
    }

    #[cfg(test)]
    pub fn set_capability_for_test(&mut self, cap: AvatarGenerationCapability, cx: &App) {
        let pref = crate::settings::current(cx)
            .avatar_generation_agent
            .as_deref()
            .and_then(AvatarImageAgent::from_id);
        self.gen_agent = pick_avatar_agent(&cap.agents, self.gen_agent.or(pref));
        self.capability = Some(cap);
    }

    #[cfg(test)]
    pub fn set_gen_description_for_test(
        &mut self,
        desc: &str,
        lens: fn(&mut super::new_bot::NewBot) -> &mut Self,
        window: &mut Window,
        cx: &mut Context<super::new_bot::NewBot>,
    ) {
        let inp = self.ensure_gen_input(lens, window, cx);
        let owned = desc.to_owned();
        inp.update(cx, |s, cx| s.set_value(owned, window, cx));
    }

    #[must_use]
    pub fn with_generate(&self) -> bool {
        self.capability
            .as_ref()
            .is_some_and(|c| !c.agents.is_empty())
    }

    /// Face list matching Orca `pictureFaceOrder`: mascots + optional Custom + optional Generate
    /// placed just before the face that was active when the popover opened.
    #[must_use]
    pub fn faces(&self) -> Vec<PictureFace> {
        let mut out: Vec<PictureFace> = self
            .mascots
            .iter()
            .copied()
            .map(PictureFace::Mascot)
            .collect();
        let with_gen = self.with_generate();
        let custom_in_gen_slot = with_gen && self.custom.is_some();
        if self.custom.is_some() && !custom_in_gen_slot {
            let insert_at = (self.custom_after + 1).min(out.len());
            out.insert(insert_at, PictureFace::Custom);
        }
        if with_gen {
            let anchor = match self.generate_anchor {
                GenerateAnchor::Custom if self.custom.is_some() && !custom_in_gen_slot => {
                    out.iter().position(|f| *f == PictureFace::Custom)
                }
                GenerateAnchor::Custom => {
                    Some((self.custom_after + 1).min(out.len().saturating_sub(1)))
                }
                GenerateAnchor::Mascot(i) => {
                    let target = self.mascots.get(i).copied().unwrap_or(self.mascots[0]);
                    out.iter().position(|f| *f == PictureFace::Mascot(target))
                }
            }
            .unwrap_or(0);
            out.insert(anchor, PictureFace::Generate);
        }
        out
    }

    #[must_use]
    pub fn current_face(&self) -> PictureFace {
        let with_gen = self.with_generate();
        if with_gen && (self.on_generate || (self.showing_custom && self.custom.is_some())) {
            PictureFace::Generate
        } else if self.showing_custom && self.custom.is_some() {
            PictureFace::Custom
        } else {
            PictureFace::Mascot(
                self.mascots
                    .get(self.mascot_idx)
                    .copied()
                    .unwrap_or(self.mascots[0]),
            )
        }
    }

    #[must_use]
    pub fn current_image(&self) -> Option<Arc<Image>> {
        if self.showing_custom
            && let Some((_, img, _, _)) = &self.custom
        {
            return Some(img.clone());
        }
        let id = self
            .mascots
            .get(self.mascot_idx)
            .copied()
            .unwrap_or(self.mascots[0]);
        avatar::mascot_image(id, self.background)
    }

    /// Background tint for `NewBot`'s top gradient band (Orca `band`).
    #[must_use]
    pub fn band_color(&self) -> Option<u32> {
        if self.showing_custom
            && let Some((_, _, tint, _)) = &self.custom
        {
            return *tint;
        }
        Some(self.background)
    }

    /// `(avatar_field, avatar_color_field, png_upload_bytes)` for `CreateBotV2` + `UploadBotAvatar`.
    #[must_use]
    pub fn avatar_and_upload(&self) -> (Option<String>, Option<String>, Option<Vec<u8>>) {
        if self.showing_custom
            && let Some((png, _, _, _)) = &self.custom
        {
            return (None, None, Some(png.clone()));
        }
        let mascot = self
            .mascots
            .get(self.mascot_idx)
            .copied()
            .unwrap_or(self.mascots[0]);
        let color = (self.background != avatar::DEFAULT_BACKGROUND)
            .then(|| avatar::format_color(self.background));
        (Some(mascot.to_owned()), color, None)
    }

    pub fn cancel_running_generation(&mut self) {
        self.gen_seq = self.gen_seq.wrapping_add(1);
        if let GenRunState::Running { ref cancel, .. } = self.gen_run {
            cancel.cancel();
            self.gen_run = GenRunState::Idle;
        }
        self.gen_task = None;
    }

    pub fn input_is_composing(&self, window: &mut Window, app: &mut App) -> bool {
        super::opt_input_composing(self.hex_input.as_ref(), window, app)
            || super::opt_textarea_composing(self.gen_input.as_ref(), window, app)
    }

    /// Whether the active picture supports mascot look controls (background swatches/outfits).
    /// Orca #47301: uploaded and generated pictures hide color/outfit controls and mascot-only notes.
    #[must_use]
    pub fn picture_takes_look(&self) -> bool {
        matches!(self.current_face(), PictureFace::Mascot(_))
    }

    pub fn open_popover<E: 'static>(
        &mut self,
        open: bool,
        lens: fn(&mut E) -> &mut Self,
        cx: &mut Context<E>,
    ) {
        self.popover_open = open;
        if open {
            self.on_generate = self.with_generate() && self.showing_custom && self.custom.is_some();
            self.generate_anchor = if self.showing_custom && self.custom.is_some() {
                GenerateAnchor::Custom
            } else {
                GenerateAnchor::Mascot(self.mascot_idx)
            };
            if matches!(self.intake, IntakeLine::Error(_)) {
                self.intake = IntakeLine::Hint;
            }
            self.refresh_capability(lens, cx);
        } else {
            self.hex_open = false;
            self.gen_editing = false;
            self.cancel_running_generation();
            // Orca #47278: drop superseded upload snapshots and unused generated images once closed.
            if matches!(self.intake, IntakeLine::Saved { .. }) {
                self.intake = IntakeLine::Hint;
            }
            if !self.showing_custom {
                self.gen_made = None;
            }
        }
        cx.notify();
    }

    pub fn step<E: 'static>(&mut self, delta: isize, cx: &mut Context<E>) {
        let order = self.faces();
        if order.is_empty() {
            return;
        }
        let cur = order
            .iter()
            .position(|f| *f == self.current_face())
            .unwrap_or(0);
        let next = (cur.cast_signed() + delta)
            .rem_euclid(order.len().cast_signed())
            .cast_unsigned();
        self.select_face_value(order[next], cx);
    }

    pub fn select_face_at<E: 'static>(&mut self, index: usize, cx: &mut Context<E>) {
        let order = self.faces();
        if let Some(&face) = order.get(index) {
            self.select_face_value(face, cx);
        }
    }

    fn select_face_value<E: 'static>(&mut self, face: PictureFace, cx: &mut Context<E>) {
        self.gen_editing = false;
        self.hex_open = false;
        let selects_picture = face != PictureFace::Generate || self.gen_slot_picture().is_some();
        if selects_picture || matches!(self.intake, IntakeLine::Error(_)) {
            self.intake = IntakeLine::Hint;
        }
        if selects_picture {
            self.intake_seq += 1;
            self.reading = false;
            self.cancel_running_generation();
            if matches!(self.gen_run, GenRunState::Error { .. }) {
                self.gen_run = GenRunState::Idle;
            }
        }
        match face {
            PictureFace::Mascot(id) => {
                self.on_generate = false;
                self.showing_custom = false;
                if let Some(pos) = self.mascots.iter().position(|m| *m == id) {
                    self.mascot_idx = pos;
                }
            }
            PictureFace::Custom => {
                self.on_generate = false;
                if self.custom.is_some() {
                    self.showing_custom = true;
                }
            }
            PictureFace::Generate => {
                self.on_generate = true;
                if let Some(made) = self.gen_made.clone() {
                    self.custom = Some(made);
                    self.showing_custom = true;
                } else if self.custom.is_some() {
                    self.showing_custom = true;
                }
            }
        }
        cx.notify();
    }

    pub fn set_background<E: 'static>(&mut self, rgb: u32, cx: &mut Context<E>) {
        self.background = rgb;
        self.showing_custom = false;
        self.on_generate = false;
        cx.notify();
    }

    pub fn pick_picture<E: 'static>(lens: fn(&mut E) -> &mut Self, cx: &mut Context<E>) {
        let chosen = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some(text::NB_UPLOAD.get().into()),
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = chosen.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            this.update(cx, |this, cx| {
                lens(this).load_picture_from_path(path, lens, cx);
            })
            .ok();
        })
        .detach();
    }

    pub fn load_picture_from_path<E: 'static>(
        &mut self,
        path: PathBuf,
        lens: fn(&mut E) -> &mut Self,
        cx: &mut Context<E>,
    ) {
        self.load_picture(
            move || {
                let len = std::fs::metadata(&path)
                    .map_err(|_| PictureError::Unreadable)?
                    .len();
                if usize::try_from(len).is_ok_and(|n| n <= PICTURE_MAX_BYTES) {
                    std::fs::read(path).map_err(|_| PictureError::Unreadable)
                } else {
                    Err(PictureError::TooBig)
                }
            },
            lens,
            cx,
        );
    }

    #[must_use]
    pub const fn is_reading(&self) -> bool {
        self.reading
    }

    pub fn load_picture<E: 'static>(
        &mut self,
        read: impl FnOnce() -> Result<Vec<u8>, PictureError> + Send + 'static,
        lens: fn(&mut E) -> &mut Self,
        cx: &mut Context<E>,
    ) {
        self.intake_seq += 1;
        self.reading = true;
        cx.notify();
        let turn = self.intake_seq;
        self.picture_task = Some(cx.spawn(async move |this, cx| {
            let prepared = cx
                .background_executor()
                .spawn(async move {
                    let png = prepare_picture(&read()?)?;
                    let tint = avatar::picture_tint(&png);
                    Ok::<_, PictureError>((png, tint))
                })
                .await;
            this.update(cx, |this, cx| {
                let state = lens(this);
                if state.intake_seq != turn {
                    return;
                }
                state.reading = false;
                match prepared {
                    Ok((png, tint)) => {
                        let prev = state.custom.take();
                        let prev_was_custom = state.showing_custom;
                        let prev_mascot = state.mascot_idx;
                        let prev_bg = state.background;
                        let image =
                            Arc::new(Image::from_bytes(gpui::ImageFormat::Png, png.clone()));
                        state.custom_after = state.mascot_idx;
                        state.custom =
                            Some((png, image, tint, text::NB_YOUR_IMAGE.get().to_owned()));
                        state.showing_custom = true;
                        state.on_generate = false;
                        state.gen_made = None;
                        state.intake = IntakeLine::Saved {
                            prev_custom: prev,
                            prev_was_custom,
                            prev_mascot,
                            prev_bg,
                        };
                    }
                    Err(err) => {
                        state.intake = IntakeLine::Error(err);
                    }
                }
                cx.notify();
            })
            .ok();
        }));
    }

    pub fn undo_intake<E: 'static>(&mut self, cx: &mut Context<E>) {
        if let IntakeLine::Saved {
            prev_custom,
            prev_was_custom,
            prev_mascot,
            prev_bg,
        } = std::mem::replace(&mut self.intake, IntakeLine::Hint)
        {
            self.cancel_running_generation();
            self.custom = prev_custom;
            self.showing_custom = prev_was_custom && self.custom.is_some();
            self.mascot_idx = prev_mascot;
            self.background = prev_bg;
            self.gen_made = None;
            cx.notify();
        }
    }

    /// Handles clipboard paste if clipboard carries an image; returns `true` if handled.
    pub fn paste_image<E: 'static>(
        &mut self,
        lens: fn(&mut E) -> &mut Self,
        cx: &mut Context<E>,
    ) -> bool {
        let Some(item) = cx.read_from_clipboard() else {
            return false;
        };
        let Some(bytes) = item.entries().iter().find_map(|e| match e {
            gpui::ClipboardEntry::Image(img) if !img.bytes.is_empty() => Some(img.bytes.clone()),
            _ => None,
        }) else {
            return false;
        };
        self.load_picture(move || Ok(bytes), lens, cx);
        true
    }

    pub fn drop_paths<E: 'static>(
        &mut self,
        paths: &ExternalPaths,
        lens: fn(&mut E) -> &mut Self,
        cx: &mut Context<E>,
    ) {
        if let Some(first) = paths.paths().first() {
            self.load_picture_from_path(first.clone(), lens, cx);
        }
    }

    #[must_use]
    pub fn intake_error(&self) -> Option<PictureError> {
        match self.intake {
            IntakeLine::Error(err) => Some(err),
            _ => None,
        }
    }

    fn gen_slot_picture(&self) -> Option<(&Arc<Image>, &str)> {
        if let Some((_, img, _, desc)) = &self.gen_made {
            return Some((img, desc.as_str()));
        }
        if let Some((_, img, _, desc)) = &self.custom {
            return Some((img, desc.as_str()));
        }
        None
    }

    fn is_gen_folded(&self) -> bool {
        self.gen_slot_picture().is_some()
            && !self.gen_editing
            && !matches!(self.gen_run, GenRunState::Running { .. })
    }

    pub fn start_generation<E: 'static>(
        &mut self,
        lens: fn(&mut E) -> &mut Self,
        cx: &mut Context<E>,
    ) {
        let Some(input) = self.gen_input.as_ref() else {
            return;
        };
        let description = input.read(cx).value().trim().to_owned();
        if description.is_empty() || matches!(self.gen_run, GenRunState::Running { .. }) {
            return;
        }
        let Some(agent) = self.gen_agent else {
            self.gen_run = GenRunState::Error {
                failure: AvatarGenerationFailure::Unavailable,
                agent: None,
            };
            cx.notify();
            return;
        };
        if avatar_gen::describes_real_person(&description) {
            self.gen_run = GenRunState::Error {
                failure: AvatarGenerationFailure::RealPerson,
                agent: Some(agent),
            };
            cx.notify();
            return;
        }
        let Some(status) = self
            .capability
            .as_ref()
            .and_then(|c| c.agents.iter().find(|e| e.agent == agent))
        else {
            self.gen_run = GenRunState::Error {
                failure: AvatarGenerationFailure::Unavailable,
                agent: Some(agent),
            };
            cx.notify();
            return;
        };
        if status.status == avatar_gen::AvatarAgentStatusKind::SignedOut {
            self.gen_run = GenRunState::Error {
                failure: AvatarGenerationFailure::SignedOut,
                agent: Some(agent),
            };
            cx.notify();
            return;
        }

        self.intake_seq += 1;
        let turn = self.intake_seq;
        self.gen_seq = self.gen_seq.wrapping_add(1);
        let gen_turn = self.gen_seq;
        let cancel = AvatarCancelHandle::new();
        let cancel_bg = cancel.clone();
        self.gen_editing = false;
        self.gen_run = GenRunState::Running { agent, cancel };
        cx.notify();

        let desc_bg = description.clone();
        self.gen_task = Some(cx.spawn(async move |this, cx| {
            let outcome = cx
                .background_executor()
                .spawn(async move { avatar_gen::generate_avatar_sync(agent, &desc_bg, &cancel_bg) })
                .await;
            this.update(cx, |this, cx| {
                let state = lens(this);
                let should_refresh =
                    state.apply_generation_result(gen_turn, turn, &description, outcome);
                if should_refresh {
                    state.refresh_capability(lens, cx);
                }
                cx.notify();
            })
            .ok();
        }));
    }

    /// Applies an avatar generation completion if its generation turn is still current.
    /// Returns `true` when the caller should refresh generation capability (signed-out error).
    fn apply_generation_result(
        &mut self,
        gen_turn: u64,
        intake_turn: u64,
        description: &str,
        outcome: AvatarRunResult,
    ) -> bool {
        if self.gen_seq != gen_turn {
            return false;
        }
        self.gen_task = None;
        let own_turn = self.intake_seq == intake_turn;
        match outcome {
            AvatarRunResult::Ok { agent, image } => {
                self.gen_run = GenRunState::Idle;
                if own_turn {
                    let gpui_img = Arc::new(Image::from_bytes(
                        gpui::ImageFormat::Png,
                        image.png_bytes.clone(),
                    ));
                    let tint = avatar::picture_tint(&image.png_bytes);
                    let entry = (image.png_bytes, gpui_img, tint, description.to_owned());
                    self.gen_made = Some(entry.clone());
                    self.custom = Some(entry);
                    self.custom_after = self.mascot_idx;
                    if self.on_generate {
                        self.showing_custom = true;
                    }
                }
                let _ = agent;
                false
            }
            AvatarRunResult::Err { error, agent } => {
                if !own_turn || error == AvatarGenerationFailure::Canceled {
                    self.gen_run = GenRunState::Idle;
                    false
                } else {
                    let signed_out = error == AvatarGenerationFailure::SignedOut;
                    self.gen_run = GenRunState::Error {
                        failure: error,
                        agent,
                    };
                    signed_out
                }
            }
        }
    }

    fn ensure_hex_input<E: 'static>(
        &mut self,
        lens: fn(&mut E) -> &mut Self,
        window: &mut Window,
        cx: &mut Context<E>,
    ) -> Entity<InputState> {
        if let Some(existing) = &self.hex_input {
            let input = existing.clone();
            crate::i18n::sync_input_placeholder(
                &input,
                text::COLOR_HEX_PLACEHOLDER.get(),
                window,
                cx,
            );
            return input;
        }
        let initial = avatar::format_color(self.background);
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(text::COLOR_HEX_PLACEHOLDER.get())
                .default_value(initial)
        });
        let sub = cx.subscribe_in(&input, window, move |this, inp, ev: &InputEvent, _, cx| {
            if let InputEvent::PressEnter { .. } = ev {
                let raw = inp.read(cx).value().to_string();
                if let Some(rgb) = avatar::parse_color(&raw) {
                    let s = lens(this);
                    s.hex_open = false;
                    s.set_background(rgb, cx);
                }
            }
        });
        self.hex_input = Some(input.clone());
        self.hex_sub = Some(sub);
        input
    }

    fn ensure_gen_input<E: 'static>(
        &mut self,
        lens: fn(&mut E) -> &mut Self,
        window: &mut Window,
        cx: &mut Context<E>,
    ) -> Entity<TextareaState> {
        if let Some(existing) = &self.gen_input {
            let input = existing.clone();
            crate::i18n::sync_textarea_placeholder(
                &input,
                text::AVATAR_GEN_PLACEHOLDER.get(),
                window,
                cx,
            );
            return input;
        }
        let input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(2, 6)
                .submit_on_enter(true)
                .placeholder(text::AVATAR_GEN_PLACEHOLDER.get())
        });
        let sub = cx.subscribe_in(
            &input,
            window,
            move |this, inp, ev: &InputEvent, window, cx| match ev {
                InputEvent::Change => {
                    if !super::opt_textarea_composing(Some(inp), window, cx) {
                        let val = inp.read(cx).value().to_string();
                        if val.chars().count() > MAX_AVATAR_DESCRIPTION_CHARS {
                            let clipped: String =
                                val.chars().take(MAX_AVATAR_DESCRIPTION_CHARS).collect();
                            inp.update(cx, |s, cx| s.set_value(clipped, window, cx));
                        }
                    }
                    let s = lens(this);
                    if matches!(s.gen_run, GenRunState::Error { .. }) {
                        s.gen_run = GenRunState::Idle;
                    }
                    cx.notify();
                }
                InputEvent::PressEnter {
                    secondary: false,
                    shift: false,
                } if !super::opt_textarea_composing(Some(inp), window, cx) => {
                    lens(this).start_generation(lens, cx);
                }
                _ => {}
            },
        );
        self.gen_input = Some(input.clone());
        self.gen_sub = Some(sub);
        input
    }

    fn render_gen_textarea<E: 'static>(
        input: &Entity<TextareaState>,
        running: bool,
        cx: &mut Context<E>,
    ) -> gpui::Div {
        div()
            .px_1p5()
            .pt_1p5()
            .on_action(
                cx.listener(|_, action: &gpui_component::input::Enter, _, cx| {
                    // Consume plain Enter so GPUI does not fall through to text-input `\n`,
                    // while letting Shift+Enter and Secondary+Shift+Enter propagate.
                    if action.secondary || action.shift {
                        cx.propagate();
                    }
                }),
            )
            .child(Textarea::new(input).appearance(false).disabled(running))
    }

    /// 64×64 rounded-[18px] tile + 22px bottom-right Pencil badge opening `BotPicturePicker` popover.
    pub fn render_new_bot_trigger<E: 'static>(
        &mut self,
        lens: fn(&mut E) -> &mut Self,
        p: &ui::Palette,
        cx: &mut Context<E>,
    ) -> AnyElement {
        let prefix = self.prefix;
        let trigger_id: SharedString = format!("{prefix}-picture-btn").into();
        let popover_id: SharedString = format!("{prefix}-picture-popover").into();
        let tile = match self.current_image() {
            Some(img) => gpui::img(img)
                .size(px(64.))
                .rounded(px(18.))
                .object_fit(ObjectFit::Cover)
                .into_any_element(),
            None => div()
                .size(px(64.))
                .rounded(px(18.))
                .bg(p.muted)
                .into_any_element(),
        };
        let entity = cx.entity();
        Popover::new(popover_id)
            .open(self.popover_open)
            .appearance(false)
            .on_open_change(cx.listener(move |this, open: &bool, _, cx| {
                lens(this).open_popover(*open, lens, cx);
            }))
            .trigger(PopoverTrigger(
                div()
                    .id(trigger_id.clone())
                    .debug_selector(move || trigger_id.to_string())
                    .role(gpui::Role::Button)
                    .tab_index(0)
                    .focus_visible(|d| d.border_1().border_color(p.ring))
                    .aria_label(text::NB_CHANGE_PICTURE.get())
                    .relative()
                    .size(px(64.))
                    .flex_none()
                    .cursor_pointer()
                    .rounded(px(18.))
                    .child(
                        div()
                            .size(px(64.))
                            .rounded(px(18.))
                            .bg(p.muted)
                            .border_2()
                            .border_color(p.card)
                            .overflow_hidden()
                            .child(tile),
                    )
                    .child(
                        div()
                            .absolute()
                            .right(px(-6.))
                            .bottom(px(-6.))
                            .size(px(22.))
                            .rounded_full()
                            .border_1()
                            .border_color(p.border)
                            .bg(p.popover)
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(
                                Icon::new(KitIconName::Pencil)
                                    .size(px(ui::ICON_XS))
                                    .text_color(p.muted_foreground),
                            ),
                    ),
                false,
            ))
            .content(move |_, window, cx| {
                entity.update(cx, |this, cx| {
                    let palette = ui::palette(cx);
                    lens(this).render_picker_popover(lens, palette, window, cx)
                })
            })
            .into_any_element()
    }

    /// 96×96 rounded-2xl clickable New-bot tile for `ImportBotHero`.
    pub fn render_import_face_trigger<E: 'static>(
        &mut self,
        lens: fn(&mut E) -> &mut Self,
        p: &ui::Palette,
        cx: &mut Context<E>,
    ) -> AnyElement {
        let prefix = self.prefix;
        let trigger_id: SharedString = format!("{prefix}-picture-btn").into();
        let popover_id: SharedString = format!("{prefix}-picture-popover").into();
        let tile = match self.current_image() {
            Some(img) => gpui::img(img)
                .size(px(96.))
                .rounded(ui::RADIUS_XL)
                .object_fit(ObjectFit::Cover)
                .into_any_element(),
            None => div()
                .size(px(96.))
                .rounded(ui::RADIUS_XL)
                .bg(p.muted)
                .into_any_element(),
        };
        let entity = cx.entity();
        Popover::new(popover_id)
            .open(self.popover_open)
            .appearance(false)
            .on_open_change(cx.listener(move |this, open: &bool, _, cx| {
                lens(this).open_popover(*open, lens, cx);
            }))
            .trigger(PopoverTrigger(
                div()
                    .id(trigger_id.clone())
                    .debug_selector(move || trigger_id.to_string())
                    .role(gpui::Role::Button)
                    .tab_index(0)
                    .focus_visible(|d| d.border_2().border_color(p.ring))
                    .aria_label(text::NB_CHANGE_PICTURE.get())
                    .size(px(96.))
                    .flex_none()
                    .cursor_pointer()
                    .rounded(ui::RADIUS_XL)
                    .bg(p.muted)
                    .overflow_hidden()
                    .hover(|d| d.opacity(0.9))
                    .child(tile),
                false,
            ))
            .content(move |_, window, cx| {
                entity.update(cx, |this, cx| {
                    let palette = ui::palette(cx);
                    lens(this).render_picker_popover(lens, palette, window, cx)
                })
            })
            .into_any_element()
    }

    fn render_picker_popover<E: 'static>(
        &mut self,
        lens: fn(&mut E) -> &mut Self,
        p: &ui::Palette,
        window: &mut Window,
        cx: &mut Context<E>,
    ) -> AnyElement {
        div()
            .id(SharedString::from(format!("{}-picker-panel", self.prefix)))
            .w(px(320.))
            .p_3()
            .bg(p.popover)
            .text_color(p.popover_foreground)
            .border_1()
            .border_color(p.border)
            .rounded(ui::RADIUS_MD)
            .shadow_lg()
            .occlude()
            .on_drop(cx.listener(move |this, paths: &ExternalPaths, _, cx| {
                lens(this).drop_paths(paths, lens, cx);
            }))
            .child(self.render_picker_inner(lens, true, p, window, cx))
            .into_any_element()
    }

    /// Inline `BotPicturePicker` contents (carousel + colors/generate field + upload line).
    pub fn render_picker_inner<E: 'static>(
        &mut self,
        lens: fn(&mut E) -> &mut Self,
        upload_as_link: bool,
        p: &ui::Palette,
        window: &mut Window,
        cx: &mut Context<E>,
    ) -> gpui::Div {
        let on_generate = self.current_face() == PictureFace::Generate;
        let takes_look = self.picture_takes_look();
        let gen_folded = self.is_gen_folded();
        let hide_upload_while_typing =
            on_generate && !gen_folded && matches!(self.intake, IntakeLine::Hint);

        let gen_section = if on_generate {
            Some(self.render_generate_field(lens, p, window, cx))
        } else {
            None
        };

        div()
            .flex()
            .flex_col()
            .child(self.render_carousel(lens, p, window, cx))
            .when(takes_look, |d| {
                d.child(self.render_swatches(lens, p, window, cx))
            })
            .children(gen_section)
            .when(!hide_upload_while_typing, |d| {
                d.child(self.render_intake_line(lens, upload_as_link, p, cx))
            })
    }

    fn render_carousel<E: 'static>(
        &self,
        lens: fn(&mut E) -> &mut Self,
        p: &ui::Palette,
        window: &Window,
        cx: &mut Context<E>,
    ) -> gpui::Stateful<gpui::Div> {
        let order = self.faces();
        let len = order.len().max(1);
        let cur = order
            .iter()
            .position(|f| *f == self.current_face())
            .unwrap_or(0);
        let prev = (cur + len - 1) % len;
        let next = (cur + 1) % len;
        let focused = self.carousel_focus.is_focused(window);
        let prefix = self.prefix;

        let live: SharedString = match order.get(cur).copied().unwrap_or(PictureFace::Generate) {
            PictureFace::Mascot(_) => text::NB_MASCOT_N
                .format(&[
                    ("n", &(self.mascot_idx + 1).to_string()),
                    ("total", &self.mascots.len().to_string()),
                ])
                .into(),
            PictureFace::Custom => self
                .custom
                .as_ref()
                .map_or_else(
                    || text::NB_YOUR_IMAGE.get().to_owned(),
                    |(_, _, _, d)| d.clone(),
                )
                .into(),
            PictureFace::Generate => text::AVATAR_GEN_FACE.get().into(),
        };

        let prev_id: &'static str = if prefix == "new-bot" {
            "new-bot-prev"
        } else {
            "import-bot-prev"
        };
        let next_id: &'static str = if prefix == "new-bot" {
            "new-bot-next"
        } else {
            "import-bot-next"
        };
        let carousel_id: &'static str = if prefix == "new-bot" {
            "new-bot-carousel"
        } else {
            "import-bot-carousel"
        };

        let prev_btn = ui::button_with_tooltip(
            Button::new(prev_id)
                .ghost()
                .small()
                .icon(IconName::ChevronLeft)
                .debug_selector(move || prev_id.to_owned())
                .on_click(cx.listener(move |this, _, _, cx| lens(this).step(-1, cx))),
            prev_id,
            text::NB_PREV_PICTURE.get(),
        );
        let next_btn = ui::button_with_tooltip(
            Button::new(next_id)
                .ghost()
                .small()
                .icon(IconName::ChevronRight)
                .debug_selector(move || next_id.to_owned())
                .on_click(cx.listener(move |this, _, _, cx| lens(this).step(1, cx))),
            next_id,
            text::NB_NEXT_PICTURE.get(),
        );

        div()
            .id(carousel_id)
            .debug_selector(move || carousel_id.to_owned())
            .track_focus(&self.carousel_focus)
            .role(gpui::Role::Group)
            .aria_label(text::NB_PICTURE.get())
            .on_key_down(cx.listener(move |this, e: &KeyDownEvent, _, cx| {
                let k = e.keystroke.key.as_str();
                if k == "left" {
                    cx.stop_propagation();
                    lens(this).step(-1, cx);
                } else if k == "right" {
                    cx.stop_propagation();
                    lens(this).step(1, cx);
                }
            }))
            .relative()
            .w_full()
            .h(px(PICKER_CAROUSEL_HEIGHT))
            .flex()
            .items_center()
            .justify_center()
            .gap(px(
                (2.0f32.mul_add(96., -PICKER_FACE) - PICKER_NEIGHBOUR) / 2.
            ))
            .when(len > 1, |d| {
                d.child(self.render_face_tile(
                    order[prev],
                    prev,
                    false,
                    PICKER_NEIGHBOUR,
                    lens,
                    p,
                    cx,
                ))
            })
            .child(
                div()
                    .rounded(ui::RADIUS_XL)
                    .border_2()
                    .border_color(if focused { p.ring } else { ui::TRANSPARENT })
                    .child(self.render_face_tile(order[cur], cur, true, PICKER_FACE, lens, p, cx)),
            )
            .when(len > 2, |d| {
                d.child(self.render_face_tile(
                    order[next],
                    next,
                    false,
                    PICKER_NEIGHBOUR,
                    lens,
                    p,
                    cx,
                ))
            })
            .child(
                div()
                    .absolute()
                    .left_1()
                    .top_0()
                    .bottom_0()
                    .flex()
                    .items_center()
                    .child(prev_btn),
            )
            .child(
                div()
                    .absolute()
                    .right_1()
                    .top_0()
                    .bottom_0()
                    .flex()
                    .items_center()
                    .child(next_btn),
            )
            .child(
                div()
                    .id(SharedString::from(format!("{carousel_id}-live")))
                    .absolute()
                    .size_0()
                    .overflow_hidden()
                    .aria_label(live),
            )
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "renders single carousel slot with index, size, selection state, and lens"
    )]
    fn render_face_tile<E: 'static>(
        &self,
        face: PictureFace,
        index: usize,
        current: bool,
        size: f32,
        lens: fn(&mut E) -> &mut Self,
        p: &ui::Palette,
        cx: &mut Context<E>,
    ) -> AnyElement {
        let radius = if current {
            ui::RADIUS_XL
        } else {
            ui::RADIUS_LG
        };
        let id = SharedString::from(format!("{}-face-{index}", self.prefix));
        if face == PictureFace::Generate {
            if current && let GenRunState::Running { .. } = self.gen_run {
                return div()
                    .id(id.clone())
                    .debug_selector(move || id.to_string())
                    .role(gpui::Role::Button)
                    .aria_label(text::AVATAR_GEN_CANCEL_RUNNING.get())
                    .cursor_pointer()
                    .size(px(size))
                    .rounded(radius)
                    .border_1()
                    .border_color(p.border)
                    .bg(p.muted)
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap_1p5()
                    .text_size(ui::TEXT_XS)
                    .text_color(p.foreground)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        lens(this).cancel_running_generation();
                        cx.notify();
                    }))
                    .child(Icon::new(IconName::LoaderCircle).size(px(22.)))
                    .child(text::AVATAR_GEN_GENERATING.get())
                    .into_any_element();
            }
            if let Some((img, _)) = self.gen_slot_picture() {
                return div()
                    .id(id.clone())
                    .debug_selector(move || id.to_string())
                    .role(gpui::Role::Button)
                    .aria_label(text::AVATAR_GEN_EDIT.get())
                    .relative()
                    .flex_none()
                    .size(px(size))
                    .rounded(radius)
                    .overflow_hidden()
                    .when(!current, |d| d.opacity(0.4))
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let s = lens(this);
                        if current {
                            s.gen_editing = true;
                            cx.notify();
                        } else {
                            s.select_face_at(index, cx);
                        }
                    }))
                    .child(
                        gpui::img(img.clone())
                            .size(px(size))
                            .rounded(radius)
                            .object_fit(ObjectFit::Cover),
                    )
                    .when(current, |d| {
                        d.child(
                            div()
                                .absolute()
                                .right_1()
                                .bottom_1()
                                .size(px(22.))
                                .rounded_full()
                                .bg(p.popover.opacity(0.9))
                                .border_1()
                                .border_color(p.border)
                                .flex()
                                .items_center()
                                .justify_center()
                                .child(
                                    Icon::new(KitIconName::Pencil)
                                        .size(px(ui::ICON_XS))
                                        .text_color(p.foreground),
                                ),
                        )
                    })
                    .into_any_element();
            }
            return div()
                .id(id.clone())
                .debug_selector(move || id.to_string())
                .flex_none()
                .size(px(size))
                .rounded(radius)
                .border_1()
                .border_color(p.foreground.opacity(0.35))
                .bg(p.muted)
                .flex()
                .items_center()
                .justify_center()
                .when(!current, |d| {
                    d.opacity(0.4).cursor_pointer().on_click(
                        cx.listener(move |this, _, _, cx| lens(this).select_face_at(index, cx)),
                    )
                })
                .child(ui::icons::svg_icon(
                    ui::icons::SPARKLES_SVG,
                    if current { 28. } else { 20. },
                    p.foreground,
                ))
                .into_any_element();
        }

        let image = match face {
            PictureFace::Mascot(m) => avatar::mascot_image(m, self.background),
            PictureFace::Custom => self.custom.as_ref().map(|(_, img, _, _)| img.clone()),
            PictureFace::Generate => None,
        };
        div()
            .id(id.clone())
            .debug_selector(move || id.to_string())
            .flex_none()
            .size(px(size))
            .when(!current, |d| {
                d.opacity(0.4).cursor_pointer().on_click(
                    cx.listener(move |this, _, _, cx| lens(this).select_face_at(index, cx)),
                )
            })
            .children(image.map(|img| {
                gpui::img(img)
                    .size(px(size))
                    .rounded(radius)
                    .object_fit(ObjectFit::Cover)
            }))
            .into_any_element()
    }

    fn render_swatches<E: 'static>(
        &mut self,
        lens: fn(&mut E) -> &mut Self,
        p: &ui::Palette,
        window: &mut Window,
        cx: &mut Context<E>,
    ) -> gpui::Div {
        let prefix = self.prefix;
        let current = self.background;
        let is_custom = !ui::BOT_AVATAR_BACKGROUNDS
            .iter()
            .any(|(rgb, _)| *rgb == current);
        let mut row = div()
            .id(SharedString::from(format!("{prefix}-swatches")))
            .role(gpui::Role::Group)
            .aria_label(text::NB_BACKGROUND.get())
            .mt_2()
            .flex()
            .flex_wrap()
            .items_center()
            .justify_center()
            .gap_1();
        for (rgb, name) in ui::BOT_AVATAR_BACKGROUNDS {
            let selected = current == rgb;
            let id = SharedString::from(format!("{prefix}-swatch-{rgb:06x}"));
            let aria = avatar::background_label(rgb).unwrap_or(name);
            row = row.child(
                div()
                    .id(id.clone())
                    .debug_selector(move || id.to_string())
                    .role(gpui::Role::Button)
                    .aria_label(aria)
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
                    .on_click(cx.listener(move |this, _, _, cx| lens(this).set_background(rgb, cx)))
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
        if is_custom {
            row = row.child(
                div()
                    .size(px(20.))
                    .rounded_full()
                    .border_2()
                    .border_color(p.foreground)
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        div()
                            .size(px(16.))
                            .rounded_full()
                            .bg(ui::bot_avatar_background(current))
                            .border_1()
                            .border_color(p.border),
                    ),
            );
        }
        let custom_id = SharedString::from(format!("{prefix}-custom-color"));
        row = row.child(
            div()
                .id(custom_id.clone())
                .debug_selector(move || custom_id.to_string())
                .role(gpui::Role::Button)
                .aria_label(text::COLOR_CUSTOM.get())
                .tab_index(0)
                .size(px(18.))
                .rounded_full()
                .border_1()
                .border_color(p.border)
                .bg(p.muted)
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .on_click(cx.listener(move |this, _, window, cx| {
                    let s = lens(this);
                    s.hex_open = !s.hex_open;
                    if s.hex_open {
                        let val = avatar::format_color(s.background);
                        let inp = s.ensure_hex_input(lens, window, cx);
                        inp.update(cx, |st, cx| {
                            st.set_value(val, window, cx);
                            st.focus(window, cx);
                        });
                    }
                    cx.notify();
                }))
                .child(
                    Icon::new(IconName::Plus)
                        .size(px(ui::ICON_XS))
                        .text_color(p.muted_foreground),
                ),
        );

        let hex_strip = if self.hex_open {
            let inp = self.ensure_hex_input(lens, window, cx);
            let apply_id: SharedString = format!("{prefix}-hex-apply").into();
            let valid = avatar::parse_color(inp.read(cx).value().as_ref()).is_some();
            Some(
                div()
                    .mt_1p5()
                    .flex()
                    .items_center()
                    .justify_center()
                    .gap_1p5()
                    .child(div().w(px(108.)).child(Input::new(&inp).small()))
                    .child(
                        ui::secondary(apply_id, text::COLOR_APPLY.get())
                            .disabled(!valid)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                let s = lens(this);
                                let raw = s
                                    .hex_input
                                    .as_ref()
                                    .map(|i| i.read(cx).value().to_string())
                                    .unwrap_or_default();
                                if let Some(rgb) = avatar::parse_color(&raw) {
                                    s.hex_open = false;
                                    s.set_background(rgb, cx);
                                }
                            })),
                    ),
            )
        } else {
            None
        };

        div().flex().flex_col().child(row).children(hex_strip)
    }

    fn render_generate_field<E: 'static>(
        &mut self,
        lens: fn(&mut E) -> &mut Self,
        p: &ui::Palette,
        window: &mut Window,
        cx: &mut Context<E>,
    ) -> gpui::Div {
        let running = matches!(self.gen_run, GenRunState::Running { .. });
        let folded = self.is_gen_folded();
        if folded && let Some((_, desc)) = self.gen_slot_picture() {
            let desc_owned = desc.to_owned();
            return div().pt_2().pb_1().flex().justify_center().child(
                div()
                    .id(SharedString::from(format!("{}-gen-desc", self.prefix)))
                    .role(gpui::Role::Button)
                    .tab_index(0)
                    .aria_label(text::AVATAR_GEN_EDIT.get())
                    .cursor_pointer()
                    .text_size(ui::TEXT_XS)
                    .text_color(p.muted_foreground)
                    .hover(|d| d.text_color(p.foreground))
                    .line_clamp(2)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        let s = lens(this);
                        s.gen_editing = true;
                        let inp = s.ensure_gen_input(lens, window, cx);
                        inp.update(cx, |st, cx| st.focus(window, cx));
                        cx.notify();
                    }))
                    .child(desc_owned),
            );
        }

        let input = self.ensure_gen_input(lens, window, cx);
        let ready = !input.read(cx).value().trim().is_empty() && !running;
        let agents = self
            .capability
            .as_ref()
            .map(|c| c.agents.clone())
            .unwrap_or_default();
        let active_agent = self.gen_agent;

        let signed_out_note = if !running
            && let Some(a) = active_agent
            && agents
                .iter()
                .find(|e| e.agent == a)
                .is_some_and(|e| e.status == avatar_gen::AvatarAgentStatusKind::SignedOut)
        {
            Some((AvatarGenerationFailure::SignedOut, Some(a)))
        } else {
            None
        };
        let note = match &self.gen_run {
            GenRunState::Error { failure, agent } => Some((*failure, *agent)),
            _ => signed_out_note,
        };

        let agent_menu: AnyElement = if agents.len() >= 2
            && let Some(agent) = active_agent
        {
            let menu_btn_id: SharedString = format!("{}-gen-agent", self.prefix).into();
            let this_weak = cx.weak_entity();
            Button::new(menu_btn_id)
                .ghost()
                .xsmall()
                .disabled(running)
                .accessibility_label(
                    text::AVATAR_GEN_WITH_AGENT.format(&[("agent", &agent.label())]),
                )
                .child(ui::icons::agent_brand_icon(
                    agent.id(),
                    ui::ICON_MD,
                    p.foreground,
                ))
                .dropdown_menu(move |mut menu, _, _| {
                    for entry in &agents {
                        let a = entry.agent;
                        let w = this_weak.clone();
                        menu = menu.item(
                            PopupMenuItem::new(a.label())
                                .icon(ui::icons::agent_menu_icon(a.id()))
                                .checked(a == agent)
                                .on_click(move |_, _, cx| {
                                    crate::settings::edit(cx, move |s| {
                                        s.avatar_generation_agent = Some(a.id().to_owned());
                                    });
                                    w.update(cx, |this, cx| {
                                        let s = lens(this);
                                        s.gen_agent = Some(a);
                                        if matches!(s.gen_run, GenRunState::Error { .. }) {
                                            s.gen_run = GenRunState::Idle;
                                        }
                                        cx.notify();
                                    })
                                    .ok();
                                }),
                        );
                    }
                    menu
                })
                .into_any_element()
        } else {
            div().into_any_element()
        };

        let gen_btn_id: SharedString = format!("{}-gen-submit", self.prefix).into();
        let gen_btn = Button::new(gen_btn_id)
            .primary()
            .xsmall()
            .disabled(!ready)
            .icon(IconName::ArrowUp)
            .accessibility_label(text::AVATAR_GEN_GENERATE.get())
            .on_click(cx.listener(move |this, _, _, cx| {
                lens(this).start_generation(lens, cx);
            }));

        div()
            .pt_2()
            .pb_1()
            .flex()
            .flex_col()
            .gap_1p5()
            .child(
                div()
                    .rounded(ui::RADIUS_MD)
                    .border_1()
                    .border_color(p.border)
                    .bg(p.background)
                    .flex()
                    .flex_col()
                    .child(Self::render_gen_textarea(&input, running, cx))
                    .child(
                        div()
                            .px_1p5()
                            .pb_1p5()
                            .pt_1()
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(agent_menu)
                            .child(gen_btn),
                    ),
            )
            .children(note.map(|(failure, agent)| {
                div()
                    .flex()
                    .items_start()
                    .gap_1p5()
                    .text_size(ui::TEXT_XS)
                    .text_color(p.destructive)
                    .child(
                        Icon::new(IconName::CircleAlert)
                            .size(px(ui::ICON_MD))
                            .text_color(p.destructive),
                    )
                    .child(failure.message(agent))
            }))
    }

    fn render_intake_line<E: 'static>(
        &self,
        lens: fn(&mut E) -> &mut Self,
        upload_as_link: bool,
        p: &ui::Palette,
        cx: &mut Context<E>,
    ) -> gpui::Div {
        let upload_id = SharedString::from(format!("{}-upload", self.prefix));
        div()
            .mt_0p5()
            .h(px(28.))
            .flex()
            .items_center()
            .justify_center()
            .gap_1p5()
            .text_size(ui::TEXT_XS)
            .text_color(p.muted_foreground)
            .child(match &self.intake {
                IntakeLine::Hint => {
                    if upload_as_link {
                        div()
                            .flex()
                            .items_center()
                            .child(
                                div()
                                    .id(upload_id.clone())
                                    .debug_selector(move || upload_id.to_string())
                                    .role(gpui::Role::Button)
                                    .tab_index(0)
                                    .focus_visible(|d| d.border_1().border_color(p.ring))
                                    .cursor_pointer()
                                    .underline()
                                    .hover(|d| d.text_color(p.foreground))
                                    .on_click(cx.listener(move |_, _, _, cx| {
                                        Self::pick_picture(lens, cx);
                                    }))
                                    .child(text::NB_UPLOAD.get()),
                            )
                            .child(text::NB_UPLOAD_REST.get())
                            .into_any_element()
                    } else {
                        ui::button_with_tooltip(
                            Button::new(upload_id.clone())
                                .ghost()
                                .xsmall()
                                .debug_selector({
                                    let sel = upload_id.clone();
                                    move || sel.to_string()
                                })
                                .on_click(cx.listener(move |_, _, _, cx| {
                                    Self::pick_picture(lens, cx);
                                }))
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap_1()
                                        .child(ui::icons::svg_icon(
                                            ui::icons::IMAGE_UP_SVG,
                                            ui::ICON_SM,
                                            p.muted_foreground,
                                        ))
                                        .child(text::NB_UPLOAD_PASTE_OR_DROP.get()),
                                ),
                            upload_id,
                            text::NB_PASTE_HINT.get(),
                        )
                        .into_any_element()
                    }
                }
                IntakeLine::Saved { .. } => div()
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .child(Icon::new(IconName::Check).size(px(ui::ICON_MD)))
                    .child(text::NB_IMG_SAVED.get())
                    .child(
                        div()
                            .id(SharedString::from(format!("{}-undo", self.prefix)))
                            .role(gpui::Role::Button)
                            .tab_index(0)
                            .cursor_pointer()
                            .underline()
                            .hover(|d| d.text_color(p.foreground))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                lens(this).undo_intake(cx);
                            }))
                            .child(text::NB_IMG_UNDO.get()),
                    )
                    .into_any_element(),
                IntakeLine::Error(err) => div()
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .text_color(p.destructive)
                    .child(
                        Icon::new(IconName::CircleAlert)
                            .size(px(ui::ICON_MD))
                            .text_color(p.destructive),
                    )
                    .child(err.message())
                    .into_any_element(),
            })
    }
}

fn random_index(len: usize) -> usize {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos());
    usize::try_from(nanos).unwrap_or(0) % len.max(1)
}

impl std::fmt::Debug for PictureState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PictureState")
            .field("prefix", &self.prefix)
            .field("mascot_idx", &self.mascot_idx)
            .field("showing_custom", &self.showing_custom)
            .field("background", &self.background)
            .field("popover_open", &self.popover_open)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct DummyHost {
        picture: PictureState,
    }

    #[gpui::test]
    fn select_face_cancels_generation_and_ignores_stale_errors(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            crate::ui::apply(gpui_component::ThemeMode::Dark, cx);
            crate::settings::init(cx);
        });
        let host = cx.new(|cx| DummyHost {
            picture: PictureState::new("test", Some("A1"), cx),
        });
        cx.update(|cx| {
            host.update(cx, |h, cx| {
                let cancel = AvatarCancelHandle::new();
                h.picture.intake_seq += 1;
                let intake_turn = h.picture.intake_seq;
                h.picture.gen_seq = h.picture.gen_seq.wrapping_add(1);
                let gen_turn = h.picture.gen_seq;
                h.picture.gen_run = GenRunState::Running {
                    agent: AvatarImageAgent::Codex,
                    cancel,
                };
                h.picture.gen_task = Some(cx.spawn(async move |_, _| {}));

                // Switching to a mascot cancels gen_task and resets gen_run to Idle.
                h.picture.select_face_value(PictureFace::Mascot("B1"), cx);
                assert!(h.picture.gen_task.is_none());
                assert!(matches!(h.picture.gen_run, GenRunState::Idle));

                // A stale Err from the superseded generation must NOT overwrite gen_run.
                let refresh = h.picture.apply_generation_result(
                    gen_turn,
                    intake_turn,
                    "a blue crab",
                    AvatarRunResult::Err {
                        error: AvatarGenerationFailure::SignedOut,
                        agent: Some(AvatarImageAgent::Codex),
                    },
                );
                assert!(!refresh);
                assert!(matches!(h.picture.gen_run, GenRunState::Idle));

                // Even if gen_seq matches, a stale intake_seq (e.g. concurrent upload) drops Err.
                let cur_gen = h.picture.gen_seq;
                let old_intake = h.picture.intake_seq;
                h.picture.intake_seq += 1;
                let refresh_stale_intake = h.picture.apply_generation_result(
                    cur_gen,
                    old_intake,
                    "a blue crab",
                    AvatarRunResult::Err {
                        error: AvatarGenerationFailure::SignedOut,
                        agent: Some(AvatarImageAgent::Codex),
                    },
                );
                assert!(!refresh_stale_intake);
                assert!(matches!(h.picture.gen_run, GenRunState::Idle));
            });
        });
    }

    #[gpui::test]
    fn gen_input_submits_only_on_plain_enter_and_inserts_newline_on_shift_enter(
        cx: &mut gpui::TestAppContext,
    ) {
        struct TestView {
            picture: PictureState,
        }
        impl gpui::Render for TestView {
            fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
                let inp = self
                    .picture
                    .ensure_gen_input(|v| &mut v.picture, window, cx);
                PictureState::render_gen_textarea(&inp, false, cx)
            }
        }
        cx.update(|cx| {
            gpui_component::init(cx);
            crate::ui::apply(gpui_component::ThemeMode::Dark, cx);
            crate::settings::init(cx);
        });
        let (view, vcx) = cx.add_window_view(|_, cx| TestView {
            picture: PictureState::new("test-gen", Some("A1"), cx),
        });
        vcx.run_until_parked();
        vcx.update(|window, cx| {
            view.update(cx, |v, cx| {
                let inp = v.picture.ensure_gen_input(|s| &mut s.picture, window, cx);
                inp.update(cx, |s, cx| {
                    s.set_value("blue crab".to_owned(), window, cx);
                    s.focus(window, cx);
                });
            });
        });
        vcx.run_until_parked();

        // Shift+Enter inserts a newline without starting generation.
        vcx.simulate_keystrokes("shift-enter");
        vcx.run_until_parked();
        vcx.update(|_, cx| {
            let v = view.read(cx);
            let val = v.picture.gen_input.as_ref().unwrap().read(cx).value();
            assert!(
                val.contains('\n'),
                "Shift+Enter must insert a newline in gen_input, got {val:?}"
            );
            assert_eq!(
                v.picture.gen_seq, 0,
                "Shift+Enter must not start avatar generation"
            );
        });

        // Plain Enter starts generation without inserting another newline.
        let before = vcx.update(|_, cx| {
            view.read(cx)
                .picture
                .gen_input
                .as_ref()
                .unwrap()
                .read(cx)
                .value()
                .to_string()
        });
        vcx.simulate_keystrokes("enter");
        vcx.run_until_parked();
        vcx.update(|_, cx| {
            let v = view.read(cx);
            let after = v.picture.gen_input.as_ref().unwrap().read(cx).value();
            assert_eq!(
                after.as_ref(),
                before.as_str(),
                "plain Enter with submit_on_enter must not append a newline"
            );
            assert!(
                !matches!(v.picture.gen_run, GenRunState::Idle),
                "plain Enter must trigger avatar generation"
            );
        });
    }
}
