//! 데스크톱 Shell 음성 받아쓰기 제어기(세션 채팅·봇 채팅·그룹 채팅 컴포저 공통).
//!
//! - 컴포저별 마이크 버튼·녹음 중 칩(빨간 점 + 경과 시간 + 취소 + 중지)·변환 중 상태 렌더링
//! - 설정된 단축키(기본 `Alt+Shift+Space`)로 포커스된 컴포저에서 즉시 토글 녹음
//! - 받아쓴 텍스트를 커서 위치/끝에 삽입하고 마지막 받아쓰기 원문을 기억해 전송 전 수정 시 `VoiceRequest::Feedback` 자동 전송

use std::collections::HashMap;
use std::time::{Duration, Instant};

use gpui::{
    AnyElement, Context, Entity, Focusable as _, KeyDownEvent, SharedString, Task, Window, div,
    prelude::*, px,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::TextareaState;
use gpui_component::{Disableable as _, Icon, IconName, Sizable as _};
use krill_app::i18n::catalog::settings as stext;
use krill_app::platform::audio::AudioRecorder;
use krill_app::ui;
use krill_app::voice::{self, VoiceCall};
use krill_proto::voice::{DEFAULT_DESKTOP_SHORTCUT, VoiceTargetKind};

use super::bot_group_chat::GroupRoomKey;
use super::chat::ChatKey;
use super::{PaneId, Shell};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) enum VoiceComposerTarget {
    Chat(ChatKey),
    Group(GroupRoomKey),
}

impl VoiceComposerTarget {
    fn rpc_target(&self) -> (VoiceTargetKind, String) {
        match self {
            Self::Chat(key) => (VoiceTargetKind::Session, key.1.0.to_string()),
            Self::Group(key) => (VoiceTargetKind::GroupChat, key.group_id.clone()),
        }
    }
}

pub(super) enum VoicePhase {
    Idle,
    Recording {
        target: VoiceComposerTarget,
        started_at: Instant,
        recorder: AudioRecorder,
    },
    Transcribing {
        target: VoiceComposerTarget,
    },
}

pub(super) struct ShellVoiceState {
    pub(super) phase: VoicePhase,
    pub(super) shortcut: String,
    pub(super) shortcut_loaded: bool,
    pub(super) error: Option<SharedString>,
    pub(super) last_dictated: HashMap<VoiceComposerTarget, String>,
    pub(super) call: Option<VoiceCall>,
    pub(super) ticker: Option<Task<()>>,
    pub(super) task: Option<Task<()>>,
}

impl Default for ShellVoiceState {
    fn default() -> Self {
        Self {
            phase: VoicePhase::Idle,
            shortcut: DEFAULT_DESKTOP_SHORTCUT.to_owned(),
            shortcut_loaded: false,
            error: None,
            last_dictated: HashMap::new(),
            call: None,
            ticker: None,
            task: None,
        }
    }
}

impl ShellVoiceState {
    fn caller(&self) -> VoiceCall {
        self.call
            .clone()
            .unwrap_or_else(|| voice::daemon_voice_call(None))
    }

    pub(super) fn is_recording_for(&self, target: &VoiceComposerTarget) -> bool {
        matches!(&self.phase, VoicePhase::Recording { target: t, .. } if t == target)
    }

    pub(super) fn is_transcribing_for(&self, target: &VoiceComposerTarget) -> bool {
        matches!(&self.phase, VoicePhase::Transcribing { target: t } if t == target)
    }

    pub(super) fn is_busy_elsewhere(&self, target: &VoiceComposerTarget) -> bool {
        match &self.phase {
            VoicePhase::Idle => false,
            VoicePhase::Recording { target: t, .. } | VoicePhase::Transcribing { target: t } => {
                t != target
            }
        }
    }
}

impl Shell {
    /// 데몬에 저장된 최신 `desktop_shortcut`을 비동기로 동기화한다.
    pub(super) fn ensure_voice_shortcut_synced(&mut self, cx: &mut Context<Self>) {
        if self.voice.shortcut_loaded {
            return;
        }
        self.voice.shortcut_loaded = true;
        let call = self.voice.caller();
        cx.spawn(async move |this, cx| {
            let cfg = cx
                .background_executor()
                .spawn(async move { voice::fetch_config(&call) })
                .await;
            if let Ok(cfg) = cfg {
                this.update(cx, |this, _| {
                    if !cfg.desktop_shortcut.trim().is_empty() {
                        this.voice.shortcut = cfg.desktop_shortcut;
                    }
                })
                .ok();
            }
        })
        .detach();
    }

    /// 창 키 입력이 설정된 음성 받아쓰기 단축키와 일치하면 포커스된 컴포저에서 녹음을 토글한다.
    pub(super) fn handle_voice_shortcut(
        &mut self,
        ev: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.ensure_voice_shortcut_synced(cx);
        if !voice::matches_shortcut(&ev.keystroke, &self.voice.shortcut) {
            return false;
        }
        // 이미 녹음 중이면 어느 대상이든 즉시 중지하고 변환한다.
        if let VoicePhase::Recording { target, .. } = &self.voice.phase {
            let target = target.clone();
            self.stop_voice_dictation(&target, false, window, cx);
            return true;
        }
        if matches!(self.voice.phase, VoicePhase::Transcribing { .. }) {
            return true;
        }
        if let Some(target) = self.focused_voice_target(window, cx) {
            self.start_voice_dictation(target, window, cx);
            return true;
        }
        false
    }

    fn focused_voice_target(
        &self,
        window: &Window,
        cx: &Context<Self>,
    ) -> Option<VoiceComposerTarget> {
        // 1. 포커스된 그룹 채팅 컴포저 우선
        for (key, room) in &self.bot_ui.group_chat.rooms {
            if let Some((input, _)) = &room.composer
                && input.read(cx).focus_handle(cx).is_focused(window)
            {
                return Some(VoiceComposerTarget::Group(key.clone()));
            }
        }
        // 2. 포커스된 세션/봇 채팅 컴포저
        for (key, (input, _)) in &self.chat.inputs {
            if input.read(cx).focus_handle(cx).is_focused(window) {
                return Some(VoiceComposerTarget::Chat(key.clone()));
            }
        }
        // 3. 활성 칸이 채팅 모드면 그 칸을 기본 대상으로 삼는다.
        if let Some(pane) = self.layout.focused()
            && let Some(key) = self.chat_key(pane)
            && self.chat.inputs.contains_key(&key)
        {
            return Some(VoiceComposerTarget::Chat(key));
        }
        None
    }

    fn start_voice_dictation(
        &mut self,
        target: VoiceComposerTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !matches!(self.voice.phase, VoicePhase::Idle) {
            return;
        }
        let (kind, tid) = target.rpc_target();
        let warm_call = self.voice.caller();
        cx.background_executor()
            .spawn(async move {
                voice::send_warm(&warm_call, Some(kind), Some(tid)).ok();
            })
            .detach();

        match AudioRecorder::start() {
            Ok(recorder) => {
                self.voice.error = None;
                self.voice.phase = VoicePhase::Recording {
                    target,
                    started_at: Instant::now(),
                    recorder,
                };
                self.voice.ticker = Some(cx.spawn_in(window, async move |this, cx| {
                    loop {
                        cx.background_executor()
                            .timer(Duration::from_millis(500))
                            .await;
                        let keep_going = this
                            .update_in(cx, |this, window, cx| match &this.voice.phase {
                                VoicePhase::Recording {
                                    target, started_at, ..
                                } => {
                                    if started_at.elapsed() >= Duration::from_secs(60) {
                                        let t = target.clone();
                                        this.stop_voice_dictation(&t, false, window, cx);
                                        false
                                    } else {
                                        cx.notify();
                                        true
                                    }
                                }
                                _ => false,
                            })
                            .unwrap_or(false);
                        if !keep_going {
                            break;
                        }
                    }
                }));
                cx.notify();
            }
            Err(err) => {
                self.notice = Some(err.clone());
                self.voice.error = Some(SharedString::from(err));
                cx.notify();
            }
        }
    }

    fn stop_voice_dictation(
        &mut self,
        expected: &VoiceComposerTarget,
        cancel: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.voice.ticker = None;
        let prev = std::mem::replace(&mut self.voice.phase, VoicePhase::Idle);
        let VoicePhase::Recording {
            target, recorder, ..
        } = prev
        else {
            self.voice.phase = prev;
            return;
        };
        if &target != expected {
            self.voice.phase = VoicePhase::Recording {
                target,
                started_at: Instant::now(),
                recorder,
            };
            return;
        }
        if cancel {
            recorder.cancel();
            self.voice.error = None;
            cx.notify();
            return;
        }
        let wav_bytes = match recorder.stop() {
            Ok(bytes) => bytes,
            Err(err) => {
                self.notice = Some(err.clone());
                self.voice.error = Some(SharedString::from(err));
                cx.notify();
                return;
            }
        };
        self.voice.phase = VoicePhase::Transcribing {
            target: target.clone(),
        };
        self.voice.error = None;
        cx.notify();

        let (kind, tid) = target.rpc_target();
        let call = self.voice.caller();
        self.voice.task = Some(cx.spawn_in(window, async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move {
                    voice::transcribe_wav(&call, &wav_bytes, Some(kind), Some(tid), None)
                })
                .await;
            this.update_in(cx, |this, window, cx| {
                this.voice.phase = VoicePhase::Idle;
                match res {
                    Ok(out) => {
                        let text = out.text.trim().to_owned();
                        if !out.silence && !text.is_empty() {
                            this.insert_voice_text(&target, &text, window, cx);
                            this.voice.last_dictated.insert(target, text);
                        }
                    }
                    Err(err) => {
                        let msg = err.to_string();
                        this.notice = Some(msg.clone());
                        this.voice.error = Some(SharedString::from(msg));
                    }
                }
                cx.notify();
            })
            .ok();
        }));
    }

    fn insert_voice_text(
        &mut self,
        target: &VoiceComposerTarget,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let input: Option<Entity<TextareaState>> = match target {
            VoiceComposerTarget::Chat(key) => {
                self.chat.inputs.get(key).map(|(input, _)| input.clone())
            }
            VoiceComposerTarget::Group(key) => self
                .bot_ui
                .group_chat
                .rooms
                .get(key)
                .and_then(|r| r.composer.as_ref().map(|(input, _)| input.clone())),
        };
        let Some(input) = input else {
            return;
        };
        let updated = input.update(cx, |state, cx| {
            let cur = state.value().to_string();
            let next = if cur.is_empty() {
                text.to_owned()
            } else if cur.ends_with(char::is_whitespace) {
                format!("{cur}{text}")
            } else {
                format!("{cur} {text}")
            };
            state.set_value(next.clone(), window, cx);
            state.focus(window, cx);
            next
        });
        if let VoiceComposerTarget::Group(key) = target
            && let Some(room) = self.bot_ui.group_chat.rooms.get_mut(key)
        {
            room.draft = updated;
        }
    }

    /// 전송 직전 호출: 직전 받아쓰기 텍스트와 다르면 비동기로 `VoiceRequest::Feedback`을 전송한다.
    pub(super) fn notify_voice_sent(
        &mut self,
        target: &VoiceComposerTarget,
        final_text: &str,
        cx: &mut Context<Self>,
    ) {
        let Some(before) = self.voice.last_dictated.remove(target) else {
            return;
        };
        let after = final_text.trim().to_owned();
        if after.is_empty() || after == before.trim() {
            return;
        }
        let (kind, tid) = target.rpc_target();
        let call = self.voice.caller();
        cx.background_executor()
            .spawn(async move {
                voice::send_feedback(&call, before, after, Some(kind), Some(tid)).ok();
            })
            .detach();
    }

    /// 세션/봇 채팅 컴포저 및 그룹 채팅 컴포저에서 공유하는 마이크 버튼·녹음 컨트롤 렌더러.
    pub(super) fn render_voice_composer_control(
        &self,
        id_suffix: &str,
        target: VoiceComposerTarget,
        disabled: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let p = ui::palette(cx);
        let is_rec = self.voice.is_recording_for(&target);
        let is_busy = self.voice.is_transcribing_for(&target);
        let busy_elsewhere = self.voice.is_busy_elsewhere(&target);

        if is_rec {
            let secs = match &self.voice.phase {
                VoicePhase::Recording { started_at, .. } => started_at.elapsed().as_secs(),
                _ => 0,
            };
            let timer_text = format!("{}:{:02}", secs / 60, secs % 60);
            let cancel_target = target.clone();
            let stop_target = target;
            return div()
                .id(SharedString::from(format!("voice-rec-{id_suffix}")))
                .debug_selector(|| "composer-voice-recording".into())
                .flex()
                .items_center()
                .gap_1()
                .px_2()
                .py_0p5()
                .rounded_full()
                .border_1()
                .border_color(p.destructive.opacity(0.4))
                .bg(p.destructive.opacity(0.12))
                .child(div().size(px(7.)).rounded_full().bg(p.destructive))
                .child(
                    div()
                        .text_size(ui::TEXT_XS)
                        .text_color(p.foreground)
                        .child(timer_text),
                )
                .child(
                    Button::new(SharedString::from(format!("voice-cancel-{id_suffix}")))
                        .debug_selector(|| "composer-voice-cancel".into())
                        .icon(IconName::Close)
                        .tooltip(stext::VOICE_CANCEL_TOOLTIP.get())
                        .xsmall()
                        .ghost()
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.stop_voice_dictation(&cancel_target, true, window, cx);
                        })),
                )
                .child(
                    Button::new(SharedString::from(format!("voice-stop-{id_suffix}")))
                        .debug_selector(|| "composer-voice-stop".into())
                        .icon(Icon::default().data(ui::icons::STOP_SQUARE_SVG))
                        .tooltip(stext::VOICE_STOP_TOOLTIP.get())
                        .xsmall()
                        .primary()
                        .rounded_full()
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.stop_voice_dictation(&stop_target, false, window, cx);
                        })),
                )
                .into_any_element();
        }

        if is_busy {
            return div()
                .id(SharedString::from(format!("voice-busy-{id_suffix}")))
                .debug_selector(|| "composer-voice-transcribing".into())
                .px_2()
                .py_0p5()
                .rounded_full()
                .bg(p.muted)
                .text_size(ui::TEXT_XS)
                .text_color(p.muted_foreground)
                .child(stext::VOICE_TRANSCRIBING.get())
                .into_any_element();
        }

        let start_target = target;
        let shortcut_tip = format!(
            "{} ({})",
            stext::VOICE_START_TOOLTIP.get(),
            voice::display_shortcut(&self.voice.shortcut)
        );
        Button::new(SharedString::from(format!("voice-mic-{id_suffix}")))
            .debug_selector(|| "composer-voice-mic".into())
            .icon(Icon::default().data(ui::icons::MIC_SVG))
            .tooltip(shortcut_tip)
            .xsmall()
            .ghost()
            .disabled(disabled || busy_elsewhere)
            .on_click(cx.listener(move |this, _, window, cx| {
                this.start_voice_dictation(start_target.clone(), window, cx);
            }))
            .into_any_element()
    }

    pub(super) fn render_chat_voice_control(
        &self,
        pane: PaneId,
        key: &ChatKey,
        disabled: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        self.render_voice_composer_control(
            &format!("chat-{}", pane.get()),
            VoiceComposerTarget::Chat(key.clone()),
            disabled,
            cx,
        )
    }

    pub(super) fn render_group_voice_control(
        &self,
        key: &GroupRoomKey,
        disabled: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        self.render_voice_composer_control(
            &format!("group-{}", key.group_id),
            VoiceComposerTarget::Group(key.clone()),
            disabled,
            cx,
        )
    }
}
