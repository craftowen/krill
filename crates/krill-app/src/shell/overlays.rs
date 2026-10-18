//! OWE-903: 온보딩·기기 연결(짝짓기) 인앱 모달 호스팅.
//!
//! 별도 OS 창(`cx.open_window`) 대신 메인 창 안 `ui::modal_backdrop` 모달로 연다.
//! 라이브러리(`link_ui`·`onboarding`)가 호스트 요청을 디스패치하면 메인 `Shell`이
//! 모달 엔티티를 들고 렌더에서 함께 그린다. `dispatch`는 `&mut App`만 받아 창이
//! 없어도 되므로, 실제 엔티티 생성은 렌더(`window`·`Context`가 있는 곳)에서 지연한다.

use std::sync::Arc;

use gpui::{AppContext as _, Context, Entity, Subscription, Window};
use krill_app::{link_ui, onboarding};

/// 짝짓기 모달 슬롯.
pub(super) struct PairingModal {
    pub(super) view: Entity<link_ui::PairingView>,
    _close: Subscription,
}

/// 온보딩 모달 슬롯.
pub(super) struct OnboardingModal {
    pub(super) view: Entity<onboarding::welcome::Wizard>,
    _close: Subscription,
}

/// 렌더에서 지연 생성할 모달 요청. 호스트 디스패치는 `&mut App`만 받아
/// `InputState` 생성에 필요한 `&mut Window`가 없으므로 일단 적어 둔다.
#[derive(Default)]
pub(super) struct PendingOverlays {
    pub(super) pairing: bool,
    pub(super) onboarding: Option<onboarding::HostCommand>,
}

impl super::Shell {
    /// 짝짓기·온보딩 모달 호스트를 등록한다. `Shell::new`에서 설정 호스트 옆에 둔다.
    pub(super) fn register_overlay_hosts(cx: &mut Context<Self>) {
        let weak = cx.weak_entity();
        link_ui::register_host(cx, move |cmd, cx| {
            let entity = weak.upgrade()?;
            if entity.read(cx).role == super::WindowRole::Floating {
                return Some(false);
            }
            match cmd {
                link_ui::HostCommand::Open => {}
            }
            Some(entity.update(cx, |this, cx| {
                this.request_pairing(cx);
                true
            }))
        });
        let weak = cx.weak_entity();
        onboarding::register_host(cx, move |cmd, cx| {
            let entity = weak.upgrade()?;
            if entity.read(cx).role == super::WindowRole::Floating {
                return Some(false);
            }
            Some(entity.update(cx, |this, cx| {
                this.request_onboarding(cmd, cx);
                true
            }))
        });
    }

    /// 렌더 시작 때 밀린 모달 요청을 실제 엔티티로 만든다.
    pub(super) fn open_pending_overlays(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.pending.pairing {
            self.pending.pairing = false;
            self.open_pairing_modal(window, cx);
        }
        if let Some(onboarding::HostCommand::Open {
            env,
            start,
            consent_only,
        }) = self.pending.onboarding.take()
        {
            // Rerun은 요청 시점에 열린 모달을 재시작하므로 여기까지 남지 않는다.
            self.open_onboarding_modal(env, start, consent_only, window, cx);
        }
    }

    /// 짝짓기 모달 요청. 열려 있으면 앞으로 가져오고, 닫혀 있으면 렌더에서 연다.
    pub(super) fn request_pairing(&mut self, cx: &mut Context<Self>) {
        if self.pairing.is_some() {
            self.activate_host_window(cx);
            return;
        }
        self.pending.pairing = true;
        cx.notify();
    }

    /// 온보딩 모달 요청. 명시적 재실행(`Rerun`)은 열린 모달을 동의부터 다시 시작한다.
    pub(super) fn request_onboarding(
        &mut self,
        cmd: &onboarding::HostCommand,
        cx: &mut Context<Self>,
    ) {
        match cmd {
            onboarding::HostCommand::Open {
                env,
                start,
                consent_only,
            } => {
                if self.onboarding.is_some() {
                    self.activate_host_window(cx);
                    return;
                }
                self.pending.onboarding = Some(onboarding::HostCommand::Open {
                    env: env.clone(),
                    start: *start,
                    consent_only: *consent_only,
                });
                cx.notify();
            }
            onboarding::HostCommand::Rerun { env } => {
                if let Some(modal) = &self.onboarding {
                    let view = modal.view.clone();
                    view.update(cx, onboarding::welcome::Wizard::restart_to_consent);
                    self.activate_host_window(cx);
                    cx.notify();
                } else {
                    self.pending.onboarding = Some(onboarding::HostCommand::Open {
                        env: env.clone(),
                        start: onboarding::state::Step::Consent,
                        consent_only: false,
                    });
                    cx.notify();
                }
            }
        }
    }

    fn activate_host_window(&self, cx: &mut Context<Self>) {
        if let Some(host_window) = self.window {
            cx.defer(move |cx| {
                host_window
                    .update(cx, |_, window, _| window.activate_window())
                    .ok();
            });
        }
    }

    /// 짝짓기 모달 엔티티를 만들고 닫기 이벤트를 구독한다.
    pub(super) fn open_pairing_modal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.pairing.is_some() {
            return;
        }
        let Some(state_dir) = krill_app::settings::state_dir(cx) else {
            return;
        };
        let executable = krill_proto::daemon::sibling_exe("krill-link")
            .unwrap_or_else(|_| std::path::PathBuf::from("krill-link"));
        let backend: Arc<dyn link_ui::LinkBackend> =
            Arc::new(link_ui::real::RealLinkBackend::new(state_dir, executable));
        let view = cx.new(|cx| link_ui::PairingView::new(backend, window, cx));
        let close = cx.subscribe_in(&view, window, |this, _, ev: &link_ui::Event, window, cx| {
            if *ev == link_ui::Event::Close {
                this.pairing = None;
                window.focus(&this.focus, cx);
                cx.notify();
            }
        });
        self.pairing = Some(PairingModal {
            view,
            _close: close,
        });
        cx.notify();
    }

    /// 온보딩 모달 엔티티를 만들고 수집 시작·재포커스 재검사·닫기를 연결한다.
    pub(super) fn open_onboarding_modal(
        &mut self,
        env: onboarding::Env,
        start: onboarding::state::Step,
        consent_only: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.onboarding.is_some() {
            return;
        }
        let view = cx.new(|cx| onboarding::welcome::Wizard::new(env, start, consent_only, cx));
        view.update(cx, |w, cx| {
            w.mount_consent(cx);
            w.observe_activation(window, cx);
            w.begin_collect(cx);
        });
        let close = cx.subscribe_in(
            &view,
            window,
            |this, _, ev: &onboarding::welcome::Event, window, cx| {
                if *ev == onboarding::welcome::Event::Close {
                    this.onboarding = None;
                    window.focus(&this.focus, cx);
                    cx.notify();
                }
            },
        );
        let focus = view.read(cx).focus_handle();
        window.focus(&focus, cx);
        self.onboarding = Some(OnboardingModal {
            view,
            _close: close,
        });
        cx.notify();
    }
}
