//! 기본 계정 전환과 실행 중 세션 처리 정책(묻기·자동 재시작·유지), 그리고 옮길 세션을 고르는 모달.
//!
//! 모달은 별도 OS 창 없이 메인 창 위 `ui::modal_backdrop`으로 그린다.

use std::collections::{BTreeMap, BTreeSet};

use gpui::{Context, InteractiveElement as _, IntoElement, ParentElement as _, Window, div};
use gpui::{StatefulInteractiveElement as _, Styled as _, px};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::checkbox::Checkbox;
use gpui_component::{Disableable as _, Sizable as _};
use krill_app::hosts::{HostId, HostTarget};
use krill_app::i18n::catalog::session_switch as text;
use krill_app::settings::RunningSessionSwitchPolicy;
use krill_app::ui;
use krill_proto::SessionId;
use krill_proto::usage::{
    AccountsSnapshot, CantMoveReason, FEAT_ACCOUNT_SWITCH_CONTROL, FEAT_PROVIDER_DETAILS,
    ProviderKind, SessionSwitchCandidate, SessionSwitchGroup, SessionSwitchPlan, UsageRequest,
};

use super::{Link, Shell, WindowRole};

/// 전환 선택 모달 상태. `Shell`이 들고 렌더에서 함께 그린다.
pub(super) struct SessionSwitchModal {
    host: HostId,
    plan: SessionSwitchPlan,
    review: bool,
    previous_account_label: String,
    next_account_label: String,
    /// 사이드바 세션 이름(열 때 고정).
    names: BTreeMap<SessionId, String>,
    selected: BTreeSet<SessionId>,
    dont_ask_again: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Group {
    Previous,
    Elsewhere,
    CantMove,
}

impl Group {
    const fn of(group: SessionSwitchGroup) -> Self {
        match group {
            SessionSwitchGroup::OnPreviousAccount => Self::Previous,
            SessionSwitchGroup::Elsewhere => Self::Elsewhere,
            SessionSwitchGroup::CantMove(_) => Self::CantMove,
        }
    }

    const fn key(self) -> &'static str {
        match self {
            Self::Previous => "previous",
            Self::Elsewhere => "elsewhere",
            Self::CantMove => "cant-move",
        }
    }
}

const fn movable(group: SessionSwitchGroup) -> bool {
    !matches!(group, SessionSwitchGroup::CantMove(_))
}

const fn reason_label(reason: CantMoveReason) -> krill_app::i18n::Message {
    match reason {
        CantMoveReason::NoSession => text::NO_SESSION,
        CantMoveReason::UnknownAccount => text::UNKNOWN_ACCOUNT,
        CantMoveReason::OwnFolder => text::OWN_FOLDER,
        CantMoveReason::SharedSession => text::SHARED_SESSION,
        CantMoveReason::PendingHandoff => text::PENDING_HANDOFF,
    }
}

const fn reason_key(reason: CantMoveReason) -> &'static str {
    match reason {
        CantMoveReason::NoSession => "no-session",
        CantMoveReason::UnknownAccount => "unknown-account",
        CantMoveReason::OwnFolder => "own-folder",
        CantMoveReason::SharedSession => "shared-session",
        CantMoveReason::PendingHandoff => "pending-handoff",
    }
}

fn account_label(snap: &AccountsSnapshot, provider: ProviderKind, id: &str) -> String {
    snap.accounts
        .iter()
        .find(|a| a.provider == provider && a.id == id)
        .map_or_else(
            || id.to_owned(),
            |a| a.email.clone().unwrap_or_else(|| a.label.clone()),
        )
}

fn folder_name(cwd: &str) -> &str {
    let trimmed = cwd.trim_end_matches(['/', '\\']);
    trimmed
        .rsplit(['/', '\\'])
        .next()
        .filter(|leaf| !leaf.is_empty())
        .unwrap_or(cwd)
}

impl SessionSwitchModal {
    fn in_group(&self, group: Group) -> impl Iterator<Item = &SessionSwitchCandidate> {
        self.plan
            .candidates
            .iter()
            .filter(move |c| Group::of(c.group) == group)
    }

    fn toggle_group(&mut self, group: Group, checked: bool) {
        let ids: Vec<_> = self.in_group(group).map(|c| c.session).collect();
        for id in ids {
            if checked {
                self.selected.insert(id);
            } else {
                self.selected.remove(&id);
            }
        }
    }
}

impl Shell {
    /// 메인 창을 기본 계정 전환 호스트로 등록한다. 설정 탭의 전환도 같은 정책을 따른다.
    pub(super) fn register_account_switch_host(cx: &mut Context<Self>) {
        let weak = cx.weak_entity();
        krill_app::settings::register_account_switch_host(cx, move |host, provider, id, cx| {
            let entity = weak.upgrade()?;
            if entity.read(cx).role != WindowRole::Main {
                return Some(false);
            }
            // 설정 뷰의 클릭 처리 중에 불리므로 셸 갱신은 그 뒤로 미룬다.
            cx.defer(move |cx| {
                entity.update(cx, |shell, cx| {
                    shell.switch_provider_account(host, provider, id, cx);
                });
            });
            Some(true)
        });
    }

    fn account_daemon_target(&self, host: &HostId) -> Option<HostTarget> {
        if host.is_local() {
            self.paths
                .clone()
                .filter(|_| self.link == Link::Up)
                .map(HostTarget::local)
        } else {
            self.target_for_host(host)
        }
    }

    /// 기본 계정 전환. 신규 데몬이면 실행 중 세션 정책을 적용하고, 구 데몬이면 기존
    /// `SwitchAccount`(모든 세션 인계)로 바꾼다.
    pub(super) fn switch_provider_account(
        &mut self,
        host: HostId,
        provider: ProviderKind,
        account_id: String,
        cx: &mut Context<Self>,
    ) {
        let control = FEAT_ACCOUNT_SWITCH_CONTROL | FEAT_PROVIDER_DETAILS;
        let features = self.sidebar.read(cx).session_account_features(&host);
        let daemon = self.account_daemon_target(&host);
        let Some(daemon) = daemon.filter(|_| features & control == control) else {
            self.refresh_accounts_for_host(
                host,
                UsageRequest::SwitchAccount {
                    provider,
                    account_id,
                },
                cx,
            );
            return;
        };
        let policy = krill_app::settings::running_session_switch_policy(cx);
        let previous_account_id = self
            .accounts_for_host(&host)
            .accounts
            .iter()
            .find(|a| a.provider == provider && a.active)
            .map(|a| a.id.clone());
        self.refresh_accounts_for_host(
            host.clone(),
            UsageRequest::SwitchAccountOnly {
                provider,
                account_id: account_id.clone(),
            },
            cx,
        );
        if policy == RunningSessionSwitchPolicy::Keep {
            return;
        }
        // 진단은 기본 계정과 무관하므로 전환과 나란히 묻는다. 뒤이은 인계 요청은
        // 진행 중인 전환 뒤로 미뤄진다(`refresh_accounts_for_host`).
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    krill_app::accounts::inspect_session_switches(
                        &daemon,
                        provider,
                        previous_account_id,
                        account_id,
                    )
                })
                .await;
            this.update(cx, |this, cx| match result {
                Ok(plan) if policy == RunningSessionSwitchPolicy::Restart => {
                    this.restart_sessions(host, &plan, plan_movable(&plan), cx);
                }
                Ok(plan) => this.open_session_switch_modal(host, plan, false, cx),
                Err(error) => {
                    this.notice = Some(error);
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    fn restart_sessions(
        &mut self,
        host: HostId,
        plan: &SessionSwitchPlan,
        sessions: Vec<SessionId>,
        cx: &mut Context<Self>,
    ) {
        if sessions.is_empty() {
            return;
        }
        self.sidebar.update(cx, |sidebar, cx| {
            for &session in &sessions {
                sidebar.mark_session_account_pending(
                    host.clone(),
                    session,
                    plan.target_account_id.clone(),
                    cx,
                );
            }
        });
        self.refresh_accounts_for_host(
            host,
            UsageRequest::SwitchSelectedSessions {
                provider: plan.provider,
                account_id: plan.target_account_id.clone(),
                sessions,
                interrupt_busy: true,
            },
            cx,
        );
    }

    /// 옮길 세션을 고르는 모달을 연다. 후보가 없으면 열지 않는다.
    pub(super) fn open_session_switch_modal(
        &mut self,
        host: HostId,
        plan: SessionSwitchPlan,
        review: bool,
        cx: &mut Context<Self>,
    ) {
        if plan.candidates.is_empty() {
            return;
        }
        let snap = self.accounts_for_host(&host);
        let previous_account_label = plan
            .previous_account_id
            .as_deref()
            .map(|id| account_label(&snap, plan.provider, id))
            .unwrap_or_default();
        let next_account_label = account_label(&snap, plan.provider, &plan.target_account_id);
        let sidebar = self.sidebar.read(cx);
        let names = plan
            .candidates
            .iter()
            .map(|c| {
                let name = sidebar.session_on(&host, c.session).map_or_else(
                    || format!("{} #{}", c.agent, c.session.0),
                    |s| sidebar.session_label_on(&host, s).0,
                );
                (c.session, name)
            })
            .collect();
        let selected = plan
            .candidates
            .iter()
            .filter(|c| c.group == SessionSwitchGroup::OnPreviousAccount)
            .map(|c| c.session)
            .collect();
        self.session_switch = Some(SessionSwitchModal {
            host,
            plan,
            review,
            previous_account_label,
            next_account_label,
            names,
            selected,
            dont_ask_again: false,
        });
        cx.notify();
    }

    pub(super) fn close_session_switch_modal(&mut self, cx: &mut Context<Self>) {
        if self.session_switch.take().is_some() {
            cx.notify();
        }
    }

    fn confirm_session_switch_modal(&mut self, cx: &mut Context<Self>) {
        let Some(modal) = self.session_switch.take() else {
            return;
        };
        cx.notify();
        if modal.selected.is_empty() {
            return;
        }
        if modal.dont_ask_again {
            krill_app::settings::set_running_session_switch_policy(
                RunningSessionSwitchPolicy::Restart,
                cx,
            );
        }
        let sessions = modal.selected.iter().copied().collect();
        self.restart_sessions(modal.host, &modal.plan, sessions, cx);
    }

    pub(super) fn render_session_switch_modal(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<gpui::Deferred> {
        let modal = self.session_switch.as_ref()?;
        let p = ui::palette(cx);
        let next = modal.next_account_label.as_str();
        let (detail, detail_id) = if modal.review {
            (
                text::REVIEW_DETAIL.format(&[("next", next)]),
                "session-switch-detail-review",
            )
        } else {
            (
                text::DETAIL.format(&[("next", next)]),
                "session-switch-detail",
            )
        };
        let mut body = div()
            .id("session-switch-body")
            .min_h_0()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap_3();
        for group in [Group::Previous, Group::Elsewhere, Group::CantMove] {
            if let Some(section) = Self::render_switch_group(modal, group, cx) {
                body = body.child(section);
            }
        }
        let count = modal.selected.len();
        let card = div()
            .id("session-switch-modal")
            .debug_selector(|| "session-switch-modal".into())
            .role(gpui::Role::Dialog)
            .aria_label(text::TITLE.format(&[("next", next)]))
            .capture_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, _, cx| {
                if event.keystroke.key == "escape" {
                    this.close_session_switch_modal(cx);
                    cx.stop_propagation();
                }
            }))
            .occlude()
            .w(ui::DIALOG_WIDTH)
            .max_w((window.viewport_size().width - ui::SPACE_4 * 2.).max(px(0.)))
            .max_h((window.viewport_size().height - ui::SPACE_4 * 2.).max(px(0.)))
            .p_4()
            .rounded(ui::RADIUS_LG)
            .bg(p.popover)
            .text_color(p.popover_foreground)
            .border_1()
            .border_color(p.border)
            .shadow_lg()
            .font_family(ui::FONT_UI)
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .flex_none()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .text_size(ui::TEXT_LG)
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(p.foreground)
                            .child(text::TITLE.format(&[("next", next)])),
                    )
                    .child(
                        div()
                            .debug_selector(move || detail_id.into())
                            .text_size(ui::TEXT_SM)
                            .text_color(p.muted_foreground)
                            .child(detail),
                    ),
            )
            .child(body)
            .child(
                div()
                    .flex_none()
                    .debug_selector(|| "session-switch-dont-ask".into())
                    .child(
                        Checkbox::new("session-switch-dont-ask")
                            .label(text::DONT_ASK.get())
                            .checked(modal.dont_ask_again)
                            .on_click(cx.listener(|this, checked: &bool, _, cx| {
                                if let Some(modal) = this.session_switch.as_mut() {
                                    modal.dont_ask_again = *checked;
                                }
                                cx.notify();
                            })),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .flex_none()
                    .flex_wrap()
                    .justify_end()
                    .gap_2()
                    .child(
                        ui::ghost("session-switch-keep", text::KEEP.get())
                            .debug_selector(|| "session-switch-keep".into())
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.close_session_switch_modal(cx);
                            })),
                    )
                    .child(
                        Button::new("session-switch-restart")
                            .debug_selector(|| "session-switch-restart".into())
                            .small()
                            .primary()
                            .disabled(count == 0)
                            .label(text::RESTART.format(&[("count", &count.to_string())]))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.confirm_session_switch_modal(cx);
                            })),
                    ),
            );
        Some(
            ui::modal_backdrop(
                window.viewport_size(),
                ui::modal_focus_scope(card, &self.tab_dialog_focus),
            )
            .with_priority(2),
        )
    }

    fn render_switch_group(
        modal: &SessionSwitchModal,
        group: Group,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        let p = ui::palette(cx);
        let mut members: Vec<_> = modal.in_group(group).collect();
        if members.is_empty() {
            return None;
        }
        members.sort_by(|a, b| a.cwd.cmp(&b.cwd).then(a.session.0.cmp(&b.session.0)));
        let count = members.len().to_string();
        let title = match group {
            Group::Previous => text::GROUP_PREVIOUS.format(&[
                ("prev", modal.previous_account_label.as_str()),
                ("count", &count),
            ]),
            Group::Elsewhere => text::GROUP_ELSEWHERE.format(&[("count", &count)]),
            Group::CantMove => text::GROUP_CANT_MOVE.format(&[("count", &count)]),
        };
        let all_checked =
            group != Group::CantMove && members.iter().all(|c| modal.selected.contains(&c.session));
        let header_id = format!("session-switch-group-{}", group.key());
        let header = div()
            .debug_selector({
                let id = header_id.clone();
                move || id.clone()
            })
            .flex_none()
            .child(
                Checkbox::new(gpui::SharedString::from(header_id))
                    .label(title)
                    .checked(all_checked)
                    .disabled(group == Group::CantMove)
                    .on_click(cx.listener(move |this, checked: &bool, _, cx| {
                        if let Some(modal) = this.session_switch.as_mut() {
                            modal.toggle_group(group, *checked);
                        }
                        cx.notify();
                    })),
            );
        let mut section = div().flex().flex_col().gap_1().child(header);
        let mut folder: Option<&str> = None;
        for candidate in members {
            if folder != Some(candidate.cwd.as_str()) {
                folder = Some(candidate.cwd.as_str());
                section = section.child(
                    div()
                        .pl_6()
                        .pt_1()
                        .truncate()
                        .text_size(ui::TEXT_XS)
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(p.muted_foreground)
                        .child(folder_name(&candidate.cwd).to_owned()),
                );
            }
            section = section.child(Self::render_switch_row(modal, candidate, cx));
        }
        Some(section.into_any_element())
    }

    fn render_switch_row(
        modal: &SessionSwitchModal,
        candidate: &SessionSwitchCandidate,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let p = ui::palette(cx);
        let session = candidate.session;
        let row_id = format!("session-switch-row-{}", session.0);
        let name = modal.names.get(&session).cloned().unwrap_or_default();
        let (tone, state) = if candidate.busy {
            (ui::BadgeTone::Warning, text::BUSY.get())
        } else {
            (ui::BadgeTone::Muted, text::IDLE.get())
        };
        let state_id = format!(
            "session-switch-state-{}-{}",
            session.0,
            if candidate.busy { "busy" } else { "idle" }
        );
        let mut row = div()
            .pl_6()
            .min_w_0()
            .flex()
            .items_center()
            .gap_2()
            .child(
                div()
                    .debug_selector({
                        let id = row_id.clone();
                        move || id.clone()
                    })
                    .flex_none()
                    .child(
                        Checkbox::new(gpui::SharedString::from(row_id))
                            .accessibility_label(name.clone())
                            .checked(modal.selected.contains(&session))
                            .disabled(!movable(candidate.group))
                            .on_click(cx.listener(move |this, checked: &bool, _, cx| {
                                if let Some(modal) = this.session_switch.as_mut() {
                                    if *checked {
                                        modal.selected.insert(session);
                                    } else {
                                        modal.selected.remove(&session);
                                    }
                                }
                                cx.notify();
                            })),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(ui::TEXT_SM)
                    .text_color(p.foreground)
                    .child(name),
            );
        if candidate.group == SessionSwitchGroup::Elsewhere {
            row = row.child(
                div()
                    .flex_none()
                    .max_w(px(160.))
                    .truncate()
                    .text_size(ui::TEXT_XS)
                    .text_color(p.muted_foreground)
                    .child(candidate.current_account_label.clone()),
            );
        }
        if let SessionSwitchGroup::CantMove(reason) = candidate.group {
            let reason_id = format!("session-switch-reason-{}-{}", session.0, reason_key(reason));
            row = row.child(
                ui::badge(p, ui::BadgeTone::Neutral, reason_label(reason).get())
                    .debug_selector(move || reason_id.clone()),
            );
        }
        row.child(ui::badge(p, tone, state).debug_selector(move || state_id.clone()))
    }
}

fn plan_movable(plan: &SessionSwitchPlan) -> Vec<SessionId> {
    plan.candidates
        .iter()
        .filter(|c| movable(c.group))
        .map(|c| c.session)
        .collect()
}

#[cfg(test)]
#[cfg(unix)]
mod tests;
