//! 점검(온보딩) 인앱 모달과 원격 Krill 설정 페이지 진입점.
//!
//! 별도 OS 창(`cx.open_window`) 대신 메인 창 안 `ui::modal_backdrop` 모달로 연다.
//! `onboarding`이 호스트 요청을 디스패치하면 메인 `Shell`이
//! 모달 엔티티를 들고 렌더에서 함께 그린다. `dispatch`는 `&mut App`만 받아 창이
//! 없어도 되므로, 실제 엔티티 생성은 렌더(`window`·`Context`가 있는 곳)에서 지연한다.

use gpui::{AppContext as _, Context, Entity, Subscription, Window};
use krill_app::{link_ui, onboarding};

/// 점검 모달 슬롯.
pub(super) struct OnboardingModal {
    pub(super) view: Entity<onboarding::welcome::Wizard>,
    _close: Subscription,
}

/// 렌더에서 지연 생성할 모달 요청. 호스트 디스패치는 `&mut App`만 받아
/// `InputState` 생성에 필요한 `&mut Window`가 없으므로 일단 적어 둔다.
#[derive(Default)]
pub(super) struct PendingOverlays {
    pub(super) onboarding: Option<onboarding::Env>,
}

impl super::Shell {
    /// 원격 Krill 설정·온보딩 모달 호스트를 등록한다. `Shell::new`에서 설정 호스트 옆에 둔다.
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
                this.open_settings(krill_app::settings::AdminTab::RemoteKrill, None, cx);
                this.activate_host_window(cx);
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
        if let Some(env) = self.pending.onboarding.take() {
            self.open_onboarding_modal(env, window, cx);
        }
    }

    /// 점검 모달 요청. 열려 있으면 앞으로 가져오고, 닫혀 있으면 렌더에서 연다.
    pub(super) fn request_onboarding(
        &mut self,
        cmd: &onboarding::HostCommand,
        cx: &mut Context<Self>,
    ) {
        let onboarding::HostCommand::Open { env } = cmd;
        if self.onboarding.is_some() {
            self.activate_host_window(cx);
            return;
        }
        self.pending.onboarding = Some(env.clone());
        cx.notify();
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

    /// 점검 모달 엔티티를 만들고 수집 시작·재포커스 재검사·닫기를 연결한다.
    pub(super) fn open_onboarding_modal(
        &mut self,
        env: onboarding::Env,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.onboarding.is_some() {
            return;
        }
        let view = cx.new(|cx| onboarding::welcome::Wizard::new(env, cx));
        view.update(cx, |w, cx| {
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
