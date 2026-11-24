//! 설정 → 음성 / Pipito 탭:
//! - Pipito 로컬 브리지 연결 상태 (`연결됨` / `로딩 중` / `Pipito 미실행` / `Pipito 미설치`) + 새로고침
//! - 문장 다듬기(AI Refine) ON/OFF 토글(`krilld`·모바일 PWA 동기화)
//! - 데스크톱 받아쓰기 단축키 캡처·초기화(`Alt+Shift+Space`)
//! - 자동 학습 단어장(빈도 표시, 직접 추가, 삭제/무시)

use gpui::{
    Context, Entity, FontWeight, KeyDownEvent, SharedString, Task, Window, div, prelude::*,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::{Input, InputState};
use gpui_component::{Disableable as _, IconName, Sizable as _};
use krill_proto::voice::{
    DEFAULT_DESKTOP_SHORTCUT, VoiceConfig, VoiceConfigPatch, VoiceConnectionStatus, VoiceStatusInfo,
};

use super::{SettingsView, layout, text};
use crate::ui::{self, BadgeTone};
use crate::voice::{self, VoiceCall};

#[derive(Default)]
pub(super) struct VoicePanel {
    pub(super) call: Option<VoiceCall>,
    pub(super) status: Option<VoiceStatusInfo>,
    pub(super) config: VoiceConfig,
    pub(super) loading: bool,
    pub(super) saving: bool,
    pub(super) error: Option<SharedString>,
    pub(super) capturing_shortcut: bool,
    pub(super) word_input: Option<Entity<InputState>>,
    pub(super) load_task: Option<Task<()>>,
    pub(super) save_task: Option<Task<()>>,
}

impl VoicePanel {
    fn caller(&self) -> VoiceCall {
        self.call
            .clone()
            .unwrap_or_else(|| voice::daemon_voice_call(None))
    }
}

impl SettingsView {
    /// 테스트용 커스텀 `VoiceCall`을 주입한다.
    #[cfg(test)]
    pub(crate) fn set_voice_call_for_test(&mut self, call: VoiceCall) {
        self.voice.call = Some(call);
    }

    pub(super) fn voice_enter(&mut self, cx: &mut Context<Self>) {
        self.refresh_voice(cx);
    }

    pub(super) fn refresh_voice(&mut self, cx: &mut Context<Self>) {
        let call = self.voice.caller();
        self.voice.loading = true;
        self.voice.error = None;
        cx.notify();
        self.voice.load_task = Some(cx.spawn(async move |this, cx| {
            let (status_res, cfg_res) = cx
                .background_executor()
                .spawn(async move { (voice::fetch_status(&call), voice::fetch_config(&call)) })
                .await;
            this.update(cx, |this, cx| {
                this.voice.loading = false;
                match status_res {
                    Ok(st) => {
                        this.voice.status = Some(st);
                        this.voice.error = None;
                    }
                    Err(err) => {
                        this.voice.error = Some(SharedString::from(err.to_string()));
                    }
                }
                if let Ok(cfg) = cfg_res {
                    this.voice.config = cfg;
                }
                cx.notify();
            })
            .ok();
        }));
    }

    pub(super) fn apply_voice_patch(&mut self, patch: VoiceConfigPatch, cx: &mut Context<Self>) {
        let call = self.voice.caller();
        self.voice.saving = true;
        self.voice.error = None;
        cx.notify();
        self.voice.save_task = Some(cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move { voice::update_config(&call, patch) })
                .await;
            this.update(cx, |this, cx| {
                this.voice.saving = false;
                match res {
                    Ok(cfg) => {
                        if let Some(st) = &mut this.voice.status {
                            st.refine_enabled = cfg.refine_enabled;
                            st.vocab_count = u32::try_from(
                                cfg.pinned_vocab.len().saturating_add(cfg.auto_vocab.len()),
                            )
                            .unwrap_or(u32::MAX);
                        }
                        this.voice.config = cfg;
                    }
                    Err(err) => {
                        this.voice.error = Some(SharedString::from(err.to_string()));
                    }
                }
                cx.notify();
            })
            .ok();
        }));
    }

    /// 설정 창에서 음성 단축키 캡처 모드일 때 키 입력을 가로챈다. 처리했으면 `true`.
    pub(super) fn handle_voice_shortcut_key(
        &mut self,
        ev: &KeyDownEvent,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.voice.capturing_shortcut {
            return false;
        }
        if ev.keystroke.key == "escape" {
            self.voice.capturing_shortcut = false;
            cx.notify();
            return true;
        }
        if let Some(formatted) = voice::format_keystroke(&ev.keystroke) {
            self.voice.capturing_shortcut = false;
            self.voice.config.desktop_shortcut.clone_from(&formatted);
            self.apply_voice_patch(
                VoiceConfigPatch {
                    desktop_shortcut: Some(formatted),
                    ..VoiceConfigPatch::default()
                },
                cx,
            );
            return true;
        }
        // 단독 수식키 등은 계속 캡처 대기한다.
        true
    }

    pub(super) fn render_voice_tab(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let p = ui::palette(cx);
        let word_input = self
            .voice
            .word_input
            .get_or_insert_with(|| {
                cx.new(|cx| {
                    InputState::new(window, cx).placeholder(text::VOICE_ADD_WORD_PLACEHOLDER.get())
                })
            })
            .clone();
        crate::i18n::sync_input_placeholder(
            &word_input,
            text::VOICE_ADD_WORD_PLACEHOLDER.get(),
            window,
            cx,
        );

        let (badge_tone, status_label) = match self.voice.status.as_ref().map(|s| s.status) {
            Some(VoiceConnectionStatus::Ready) => {
                (BadgeTone::Success, text::VOICE_STATUS_READY.get())
            }
            Some(VoiceConnectionStatus::Loading) => {
                (BadgeTone::Warning, text::VOICE_STATUS_LOADING.get())
            }
            Some(VoiceConnectionStatus::NotRunning) => {
                (BadgeTone::Destructive, text::VOICE_STATUS_NOT_RUNNING.get())
            }
            Some(VoiceConnectionStatus::NotInstalled) => (
                BadgeTone::Destructive,
                text::VOICE_STATUS_NOT_INSTALLED.get(),
            ),
            None => (BadgeTone::Muted, text::VOICE_STATUS_CHECKING.get()),
        };

        let status_desc = match self.voice.status.as_ref() {
            Some(st) if st.status == VoiceConnectionStatus::Ready => {
                if let Some(ver) = st.app_version.as_deref() {
                    text::VOICE_STATUS_READY_DESC.format(&[("version", ver)])
                } else {
                    text::VOICE_STATUS_DESC.get().to_owned()
                }
            }
            Some(st) if st.status == VoiceConnectionStatus::NotRunning => {
                text::VOICE_STATUS_NOT_RUNNING_DESC.get().to_owned()
            }
            Some(st) if st.status == VoiceConnectionStatus::NotInstalled => {
                text::VOICE_STATUS_NOT_INSTALLED_DESC.get().to_owned()
            }
            _ => text::VOICE_STATUS_DESC.get().to_owned(),
        };

        let loading = self.voice.loading;
        let saving = self.voice.saving;
        let refine_enabled = self.voice.config.refine_enabled;
        let capturing = self.voice.capturing_shortcut;
        let shortcut_display = voice::display_shortcut(&self.voice.config.desktop_shortcut);
        let is_default_shortcut = self
            .voice
            .config
            .desktop_shortcut
            .eq_ignore_ascii_case(DEFAULT_DESKTOP_SHORTCUT);

        // 1) Pipito 연결 상태 행
        let status_row = ui::row(p)
            .child(layout::label(
                p,
                text::VOICE_PIPITO_STATUS.get(),
                status_desc,
            ))
            .child(
                ui::controls()
                    .child(ui::badge(p, badge_tone, status_label))
                    .child(
                        ui::secondary("voice-refresh-btn", text::VOICE_REFRESH.get())
                            .debug_selector(|| "voice-refresh-btn".into())
                            .disabled(loading)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.refresh_voice(cx);
                            })),
                    ),
            );

        // 2) 문장 다듬기(AI Refine) 토글 행
        let refine_entity = cx.entity();
        let refine_row = ui::row(p)
            .child(layout::label(
                p,
                text::VOICE_REFINE_TITLE.get(),
                text::VOICE_REFINE_DESC.get(),
            ))
            .child(ui::controls().child(ui::switch(
                p,
                "voice-refine-switch",
                refine_enabled,
                saving,
                move |next, _, cx| {
                    refine_entity.update(cx, |this, cx| {
                        this.voice.config.refine_enabled = next;
                        this.apply_voice_patch(
                            VoiceConfigPatch {
                                refine_enabled: Some(next),
                                ..VoiceConfigPatch::default()
                            },
                            cx,
                        );
                    });
                },
            )));

        // 3) 데스크톱 받아쓰기 단축키 캡처 행
        let shortcut_row = ui::row(p)
            .child(layout::label(
                p,
                text::VOICE_SHORTCUT_TITLE.get(),
                if capturing {
                    text::VOICE_SHORTCUT_RECORDING.get()
                } else {
                    text::VOICE_SHORTCUT_DESC.get()
                },
            ))
            .child(
                ui::controls()
                    .child(
                        div()
                            .id("voice-shortcut-capture")
                            .debug_selector(|| "voice-shortcut-capture".into())
                            .cursor_pointer()
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.voice.capturing_shortcut = !this.voice.capturing_shortcut;
                                window.focus(&this.focus, cx);
                                cx.notify();
                            }))
                            .child(if capturing {
                                ui::badge(
                                    p,
                                    BadgeTone::Warning,
                                    text::VOICE_SHORTCUT_PRESS_KEY.get(),
                                )
                            } else {
                                ui::keycap(p, shortcut_display)
                            }),
                    )
                    .when(!is_default_shortcut, |d| {
                        d.child(
                            ui::ghost("voice-shortcut-reset", text::VOICE_SHORTCUT_RESET.get())
                                .debug_selector(|| "voice-shortcut-reset".into())
                                .disabled(saving)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.voice.capturing_shortcut = false;
                                    this.apply_voice_patch(
                                        VoiceConfigPatch {
                                            desktop_shortcut: Some(
                                                DEFAULT_DESKTOP_SHORTCUT.to_owned(),
                                            ),
                                            ..VoiceConfigPatch::default()
                                        },
                                        cx,
                                    );
                                })),
                        )
                    }),
            );

        // 4) 자동 학습 단어장 카드
        let total_vocab = self
            .voice
            .config
            .pinned_vocab
            .len()
            .saturating_add(self.voice.config.auto_vocab.len());
        let vocab_title = text::VOICE_VOCAB_TITLE.format(&[("count", &total_vocab.to_string())]);

        let add_input = word_input.clone();
        let add_bar = div()
            .w_full()
            .flex()
            .items_center()
            .gap_2()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(Input::new(&word_input).small()),
            )
            .child(
                ui::secondary("voice-vocab-add", text::VOICE_ADD_WORD.get())
                    .debug_selector(|| "voice-vocab-add".into())
                    .disabled(saving)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        let word = add_input.read(cx).value().trim().to_owned();
                        if word.is_empty() {
                            return;
                        }
                        add_input
                            .update(cx, |state, cx| state.set_value(String::new(), window, cx));
                        this.apply_voice_patch(
                            VoiceConfigPatch {
                                add_pinned: Some(word),
                                ..VoiceConfigPatch::default()
                            },
                            cx,
                        );
                    })),
            );

        let pinned_words = self.voice.config.pinned_vocab.clone();
        let auto_words: Vec<_> = self
            .voice
            .config
            .auto_vocab
            .iter()
            .take(40)
            .cloned()
            .collect();
        let mut chips = div().w_full().flex().flex_wrap().gap_1p5();

        for (idx, word) in pinned_words.into_iter().enumerate() {
            let remove_word = word.clone();
            chips = chips.child(
                div()
                    .id(SharedString::from(format!("voice-pinned-{idx}")))
                    .px_2()
                    .py_0p5()
                    .rounded(ui::RADIUS_SM)
                    .border_1()
                    .border_color(p.ring)
                    .bg(p.accent)
                    .flex()
                    .items_center()
                    .gap_1()
                    .text_size(ui::TEXT_XS)
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(p.accent_foreground)
                    .child(word)
                    .child(
                        Button::new(SharedString::from(format!("voice-pinned-rm-{idx}")))
                            .icon(IconName::Close)
                            .xsmall()
                            .ghost()
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.apply_voice_patch(
                                    VoiceConfigPatch {
                                        remove_pinned: Some(remove_word.clone()),
                                        ..VoiceConfigPatch::default()
                                    },
                                    cx,
                                );
                            })),
                    ),
            );
        }

        for (idx, entry) in auto_words.into_iter().enumerate() {
            let ignore_word = entry.word.clone();
            let label = format!("{} ({})", entry.word, entry.count);
            chips = chips.child(
                div()
                    .id(SharedString::from(format!("voice-auto-{idx}")))
                    .px_2()
                    .py_0p5()
                    .rounded(ui::RADIUS_SM)
                    .border_1()
                    .border_color(p.border)
                    .bg(p.muted)
                    .flex()
                    .items_center()
                    .gap_1()
                    .text_size(ui::TEXT_XS)
                    .text_color(p.foreground)
                    .child(label)
                    .child(
                        Button::new(SharedString::from(format!("voice-auto-rm-{idx}")))
                            .icon(IconName::Close)
                            .xsmall()
                            .ghost()
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.apply_voice_patch(
                                    VoiceConfigPatch {
                                        ignore_word: Some(ignore_word.clone()),
                                        ..VoiceConfigPatch::default()
                                    },
                                    cx,
                                );
                            })),
                    ),
            );
        }

        let vocab_card = ui::card(p)
            .flex()
            .flex_col()
            .gap_3()
            .child(layout::label(p, vocab_title, text::VOICE_VOCAB_DESC.get()))
            .child(add_bar)
            .child(if total_vocab == 0 {
                div()
                    .text_size(ui::TEXT_XS)
                    .text_color(p.muted_foreground)
                    .child(text::VOICE_VOCAB_EMPTY.get())
            } else {
                chips
            });

        let mut body = div().w_full().min_w_0().flex().flex_col().gap_3();
        if let Some(err) = &self.voice.error {
            body = body.child(ui::error_banner(p, crate::i18n::cached_notice(err)));
        }
        body.child(status_row)
            .child(refine_row)
            .child(shortcut_row)
            .child(vocab_card)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use gpui::{AppContext as _, TestAppContext};
    use krill_proto::voice::{
        VocabEntry, VoiceConfig, VoiceConnectionStatus, VoiceReply, VoiceRequest, VoiceStatusInfo,
    };

    use crate::settings::{AdminTab, SettingsView};

    #[gpui::test]
    fn voice_settings_tab_loads_status_toggles_refine_and_captures_shortcut(
        cx: &mut TestAppContext,
    ) {
        let requests: Arc<Mutex<Vec<VoiceRequest>>> = Arc::new(Mutex::new(Vec::new()));
        let reqs_bg = Arc::clone(&requests);
        let state = Arc::new(Mutex::new(VoiceConfig {
            refine_enabled: true,
            desktop_shortcut: "Alt+Shift+Space".to_owned(),
            pinned_vocab: vec!["Pipito".to_owned()],
            ignored_vocab: Vec::new(),
            auto_vocab: vec![VocabEntry {
                word: "krilld".to_owned(),
                count: 5,
                last_seen_ms: 100,
            }],
        }));
        let state_bg = Arc::clone(&state);

        let call: crate::voice::VoiceCall = Arc::new(move |req| {
            reqs_bg.lock().unwrap().push(req.clone());
            let mut cfg = state_bg.lock().unwrap();
            match req {
                VoiceRequest::Status => Ok(VoiceReply::Status(VoiceStatusInfo {
                    available: true,
                    status: VoiceConnectionStatus::Ready,
                    app_version: Some("1.5.0".to_owned()),
                    engine_ready: true,
                    engine_error: None,
                    refine_available: true,
                    refine_enabled: cfg.refine_enabled,
                    vocab_count: 2,
                })),
                VoiceRequest::ConfigGet => Ok(VoiceReply::Config(cfg.clone())),
                VoiceRequest::ConfigSet(patch) => {
                    if let Some(r) = patch.refine_enabled {
                        cfg.refine_enabled = r;
                    }
                    if let Some(sc) = &patch.desktop_shortcut {
                        cfg.desktop_shortcut.clone_from(sc);
                    }
                    if let Some(w) = &patch.add_pinned {
                        cfg.pinned_vocab.push(w.clone());
                    }
                    Ok(VoiceReply::Config(cfg.clone()))
                }
                _ => Ok(VoiceReply::Warmed { ok: true }),
            }
        });

        let view = cx.new(|cx| {
            let mut v = SettingsView::new(cx);
            v.set_voice_call_for_test(call);
            v.set_tab(AdminTab::Voice, cx);
            v
        });
        cx.run_until_parked();

        view.read_with(cx, |v, _| {
            assert_eq!(v.tab(), AdminTab::Voice);
            assert_eq!(
                v.voice.status.as_ref().map(|s| s.status),
                Some(VoiceConnectionStatus::Ready)
            );
            assert!(v.voice.config.refine_enabled);
            assert_eq!(v.voice.config.pinned_vocab, vec!["Pipito"]);
            assert_eq!(v.voice.config.auto_vocab.len(), 1);
        });

        // Shortcut capture updates config via VoiceRequest::ConfigSet
        view.update(cx, |v, cx| {
            v.voice.capturing_shortcut = true;
            let ev = gpui::KeyDownEvent {
                keystroke: gpui::Keystroke {
                    modifiers: gpui::Modifiers {
                        platform: true,
                        shift: true,
                        ..gpui::Modifiers::default()
                    },
                    key: "v".to_owned(),
                    key_char: None,
                },
                is_held: false,
                prefer_character_input: false,
            };
            assert!(v.handle_voice_shortcut_key(&ev, cx));
            assert!(!v.voice.capturing_shortcut);
        });
        cx.run_until_parked();

        view.read_with(cx, |v, _| {
            assert_eq!(v.voice.config.desktop_shortcut, "Shift+Cmd+V");
        });
        assert!(requests.lock().unwrap().iter().any(|r| matches!(
            r,
            VoiceRequest::ConfigSet(p) if p.desktop_shortcut.as_deref() == Some("Shift+Cmd+V")
        )));
    }
}
