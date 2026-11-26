//! CLI·모바일·다른 호스트가 기본 계정을 바꾼 것을 스냅샷 차이로 알아채고, 실행 중 세션 정책에 맞춰
//! 알리거나 옮긴다. 이 앱의 요청([`krill_app_core::accounts::InAppSwitches`])과 데몬 자동전환은 제외한다.

use super::*;
use krill_app_core::hosts::HostId;
use krill_app_core::settings::RunningSessionSwitchPolicy;
use krill_proto::usage::{AccountsSnapshot, ProviderKind, SessionSwitchGroup, SessionSwitchPlan};

#[cfg(all(test, unix))]
mod tests;

/// 인앱 전환 표식 수명. 이보다 늦게 도착한 스냅샷은 외부 전환으로 본다.
const IN_APP_TTL: Duration = Duration::from_secs(30);

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ExternalSwitchReview {
    pub host: HostId,
    pub provider: ProviderKind,
    pub previous_account_id: String,
    pub next_account_id: String,
    pub next_account_label: String,
    pub plan: Option<SessionSwitchPlan>,
    pub auto_moved_count: usize,
    pub cant_move_count: usize,
    pub policy: RunningSessionSwitchPolicy,
}

impl ExternalSwitchReview {
    /// 새 계정으로 옮길 수 있는 세션.
    pub fn movable(&self) -> Vec<krill_proto::SessionId> {
        self.plan
            .iter()
            .flat_map(|p| &p.candidates)
            .filter(|c| {
                matches!(
                    c.group,
                    SessionSwitchGroup::OnPreviousAccount | SessionSwitchGroup::Elsewhere
                )
            })
            .map(|c| c.session)
            .collect()
    }

    /// 버튼이 있는 카드인가. 아니면 저절로 사라지는 안내다.
    pub fn actionable(&self) -> bool {
        match self.policy {
            RunningSessionSwitchPolicy::Keep => false,
            RunningSessionSwitchPolicy::Restart => self.cant_move_count > 0,
            RunningSessionSwitchPolicy::Ask => {
                !self.movable().is_empty() || self.cant_move_count > 0
            }
        }
    }

    /// 리뷰 모달(또는 일괄 전환)을 여는 버튼 라벨. 열 대상이 없으면 `None`.
    pub fn review_label(&self) -> Option<&'static str> {
        use krill_app_core::i18n::catalog::external_switch as text;
        let has_candidates = self.plan.as_ref().is_some_and(|p| !p.candidates.is_empty());
        match self.policy {
            RunningSessionSwitchPolicy::Ask if has_candidates || !self.movable().is_empty() => {
                Some(text::SWITCH_SESSIONS.get())
            }
            RunningSessionSwitchPolicy::Restart if self.cant_move_count > 0 && has_candidates => {
                Some(text::REVIEW_UNMOVED.get())
            }
            _ => None,
        }
    }

    pub fn title(&self) -> String {
        use krill_app_core::i18n::catalog::external_switch as text;
        let args = [
            ("provider", self.provider.display_name()),
            ("account", self.next_account_label.as_str()),
        ];
        match self.policy {
            RunningSessionSwitchPolicy::Keep => text::SWITCHED_KEEP.format(&args),
            RunningSessionSwitchPolicy::Restart if self.cant_move_count > 0 => {
                text::SWITCHED_CANT_MOVE.format(&[
                    ("account", self.next_account_label.as_str()),
                    ("n", &self.cant_move_count.to_string()),
                ])
            }
            RunningSessionSwitchPolicy::Restart if self.auto_moved_count > 0 => {
                text::SWITCHED_MOVED.format(&[
                    ("provider", self.provider.display_name()),
                    ("account", self.next_account_label.as_str()),
                    ("n", &self.auto_moved_count.to_string()),
                ])
            }
            _ => text::SWITCHED.format(&args),
        }
    }

    /// Ask 카드의 미이동 세션 요약.
    pub fn summary(&self) -> Option<String> {
        (self.policy == RunningSessionSwitchPolicy::Ask && self.actionable()).then(|| {
            krill_app_core::i18n::catalog::external_switch::SUMMARY.format(&[
                ("movable", &self.movable().len().to_string()),
                ("cant", &self.cant_move_count.to_string()),
            ])
        })
    }
}

#[derive(Default)]
pub(super) struct ExternalSwitchTracker {
    last_active: BTreeMap<(HostId, ProviderKind), String>,
    last_auto_switch_seq: BTreeMap<HostId, u64>,
    pub pending_review: Option<ExternalSwitchReview>,
}

impl ExternalSwitchTracker {
    /// 스냅샷의 활성 계정을 지난 관찰과 비교해 외부 전환만 돌려준다. 호스트·프로바이더의 첫 관찰은
    /// 기준만 잡는다. 같은 전환의 인앱 표식은 소비한다.
    pub fn detect(
        &mut self,
        host: &HostId,
        snap: &AccountsSnapshot,
        in_app: &mut Vec<(HostId, ProviderKind, String, Instant)>,
        now: Instant,
    ) -> Vec<ExternalSwitchReview> {
        in_app.retain(|(.., at)| now.saturating_duration_since(*at) < IN_APP_TTL);
        let last_seq = self.last_auto_switch_seq.get(host).copied();
        if let Some(max) = snap.advanced.history.iter().map(|r| r.sequence).max() {
            self.last_auto_switch_seq.insert(host.clone(), max);
        }
        let mut events = Vec::new();
        for provider in ProviderKind::ALL {
            let key = (host.clone(), provider);
            let Some(active) = snap
                .accounts
                .iter()
                .find(|a| a.provider == provider && a.active)
            else {
                self.last_active.remove(&key);
                continue;
            };
            let Some(previous) = self.last_active.insert(key, active.id.clone()) else {
                continue;
            };
            if previous == active.id {
                continue;
            }
            let matches = |(h, p, id, _): &(HostId, ProviderKind, String, Instant)| {
                h == host && *p == provider && *id == active.id
            };
            if let Some(i) = in_app.iter().position(matches) {
                in_app.remove(i);
                continue;
            }
            let automatic = snap
                .switched
                .iter()
                .any(|e| e.provider == provider && e.to_id == active.id)
                || snap.advanced.history.iter().any(|r| {
                    last_seq.is_none_or(|seq| r.sequence > seq)
                        && r.event.provider == provider
                        && r.event.to_id == active.id
                });
            if automatic {
                continue;
            }
            events.push(ExternalSwitchReview {
                host: host.clone(),
                provider,
                previous_account_id: previous,
                next_account_id: active.id.clone(),
                next_account_label: active.label.clone(),
                plan: None,
                auto_moved_count: 0,
                cant_move_count: 0,
                policy: RunningSessionSwitchPolicy::default(),
            });
        }
        events
    }
}

impl Shell {
    /// [`Self::apply_accounts_snapshot_for_host`]가 저장하기 전에 부른다.
    pub(super) fn observe_external_switches(
        &mut self,
        host: &HostId,
        snap: &AccountsSnapshot,
        cx: &mut Context<Self>,
    ) {
        let in_app = &mut cx
            .default_global::<krill_app_core::accounts::InAppSwitches>()
            .0;
        let events = self
            .external_switch
            .detect(host, snap, in_app, Instant::now());
        for review in events {
            self.inspect_external_switch(review, cx);
        }
    }

    fn inspect_external_switch(
        &mut self,
        mut review: ExternalSwitchReview,
        cx: &mut Context<Self>,
    ) {
        review.policy = krill_app_core::settings::running_session_switch_policy(cx);
        let Some(target) = self.target_for_host(&review.host) else {
            self.settle_external_switch(review, cx);
            return;
        };
        let (provider, previous, next) = (
            review.provider,
            review.previous_account_id.clone(),
            review.next_account_id.clone(),
        );
        cx.spawn(async move |this, cx| {
            let plan = cx
                .background_executor()
                .spawn(async move {
                    krill_app_core::accounts::inspect_session_switches(
                        &target,
                        provider,
                        Some(previous),
                        next,
                    )
                })
                .await;
            this.update(cx, |this, cx| {
                // 구 데몬·조회 실패도 기본 안내는 띄운다.
                review.plan = plan.ok();
                this.settle_external_switch(review, cx);
            })
            .ok();
        })
        .detach();
    }

    fn settle_external_switch(&mut self, mut review: ExternalSwitchReview, cx: &mut Context<Self>) {
        review.cant_move_count = review
            .plan
            .iter()
            .flat_map(|p| &p.candidates)
            .filter(|c| matches!(c.group, SessionSwitchGroup::CantMove(_)))
            .count();
        if review.policy == RunningSessionSwitchPolicy::Restart {
            let movable = review.movable();
            review.auto_moved_count = movable.len();
            self.switch_external_sessions(&review, movable, cx);
        }
        self.external_switch.pending_review = Some(review);
        cx.notify();
    }

    /// 외부 전환 카드의 [세션 전환]. 옮길 수 있는 세션을 한 번에 새 계정으로 인계한다.
    pub(super) fn open_external_switch_review(&mut self, cx: &mut Context<Self>) {
        let Some(review) = self.external_switch.pending_review.take() else {
            return;
        };
        match review.plan {
            Some(plan) if !plan.candidates.is_empty() => {
                self.open_session_switch_modal(review.host, plan, true, cx);
            }
            _ => {
                let movable = review.movable();
                self.switch_external_sessions(&review, movable, cx);
            }
        }
        cx.notify();
    }

    pub(super) fn dismiss_external_switch(
        &mut self,
        review: &ExternalSwitchReview,
        cx: &mut Context<Self>,
    ) {
        if self.external_switch.pending_review.as_ref() == Some(review) {
            self.external_switch.pending_review = None;
            cx.notify();
        }
    }

    fn switch_external_sessions(
        &mut self,
        review: &ExternalSwitchReview,
        sessions: Vec<krill_proto::SessionId>,
        cx: &mut Context<Self>,
    ) {
        if sessions.is_empty() {
            return;
        }
        let Some(target) = self.target_for_host(&review.host) else {
            return;
        };
        let (host, provider, account) = (
            review.host.clone(),
            review.provider,
            review.next_account_id.clone(),
        );
        cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move {
                    krill_app_core::accounts::switch_selected_sessions(
                        &target, provider, account, sessions, false,
                    )
                })
                .await;
            this.update(cx, |this, cx| match res {
                Ok(snap) => this.apply_accounts_snapshot_for_host(host, snap, cx),
                Err(e) => {
                    this.notice = Some(e);
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }
}
