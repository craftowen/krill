use super::{Call, Panel, SettingsView, layout, text};
use gpui::{Context, div, prelude::*};
use gpui_component::Disableable as _;
use krill_proto::mobile::{ConnectMode, MobileReply, MobileRequest};
use std::{sync::Arc, time::Duration};

fn account_call(call: &Call, action: &str) -> Result<serde_json::Value, String> {
    let reply = call(MobileRequest::Workflow(
        serde_json::json!({"action":action}).to_string(),
    ))?;
    let MobileReply::Workflow(json) = reply else {
        return Err("Unexpected relay account response".into());
    };
    let value: serde_json::Value = serde_json::from_str(&json).map_err(|e| e.to_string())?;
    if value.get("ok").and_then(serde_json::Value::as_bool) != Some(true) {
        return Err(value
            .get("error")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("Relay account request failed")
            .to_owned());
    }
    Ok(value.get("data").cloned().unwrap_or_default())
}
impl Panel {
    fn account_shown_values(&self) -> (bool, Option<String>, bool, Option<String>) {
        (
            self.account.required,
            self.account.email.clone(),
            self.account.pending,
            self.error.clone(),
        )
    }
    fn accept_account(&mut self, data: &serde_json::Value) {
        if let Some(required) = data
            .get("requires_sign_in")
            .and_then(serde_json::Value::as_bool)
        {
            self.account.required = required;
        }
        if let Some(signed_in) = data.get("signed_in").and_then(serde_json::Value::as_bool) {
            if signed_in && self.error.as_deref() == Some(text::ACCOUNT_ERROR.get()) {
                self.error = None;
            }
            self.account.email = signed_in.then(|| {
                data.get("email")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("")
                    .to_owned()
            });
            if !signed_in && self.mode == ConnectMode::Tunnel {
                self.offer = None;
                self.copied_url = None;
            }
        }
        self.account.pending = data
            .get("pending")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
    }
}
impl SettingsView {
    pub(super) fn mobile_account_poll(&mut self, cx: &mut Context<Self>) {
        if self.mobile.account.timer.is_some() {
            return;
        }
        self.mobile.account.timer = Some(cx.spawn(async move |this, cx| {
            loop {
                let Ok((call, poll, generation, pending)) = this.read_with(cx, |this, _| {
                    (
                        Arc::clone(&this.mobile.call),
                        !this.mobile.account.busy
                            && (this.mobile.account.pending
                                || this.visible
                                    && (this.mobile_page
                                        || this.tab == super::super::AdminTab::Mobile)),
                        this.mobile.account.generation,
                        this.mobile.account.pending,
                    )
                }) else {
                    return;
                };
                if poll {
                    let result = cx
                        .background_executor()
                        .spawn(async move {
                            account_call(
                                &call,
                                if pending {
                                    "relay_auth_poll"
                                } else {
                                    "relay_auth_status"
                                },
                            )
                        })
                        .await;
                    if this
                        .update(cx, |this, cx| {
                            if this.mobile.account.generation != generation
                                || this.mobile.account.busy
                            {
                                return;
                            }
                            let shown = this.mobile.account_shown_values();
                            match result {
                                Ok(data) => this.mobile.accept_account(&data),
                                Err(_) if pending => {
                                    // The daemon owns approval expiry. A failed IPC/poll
                                    // must not abandon an approval while this page is hidden.
                                    this.mobile.error = Some(text::ACCOUNT_ERROR.get().to_owned());
                                }
                                Err(_) => {}
                            }
                            this.mobile_resume_after_login(cx);
                            // 탭에 있는 동안 5초마다 묻는다. 값이 같으면 다시 그리지 않는다.
                            if this.mobile.account_shown_values() != shown {
                                cx.notify();
                            }
                        })
                        .is_err()
                    {
                        return;
                    }
                }
                cx.background_executor().timer(Duration::from_secs(5)).await;
            }
        }));
    }
    fn mobile_resume_after_login(&mut self, cx: &mut Context<Self>) {
        if self.mobile.account.email.is_some()
            && let Some(mode) = self.mobile.account.connect_after_login.take()
        {
            self.mobile_pair(mode, self.mobile.address.clone(), cx);
        }
    }
    pub(super) fn mobile_account_action(&mut self, logout: bool, cx: &mut Context<Self>) {
        if logout {
            self.mobile.account.connect_after_login = None;
        }
        if self.mobile.account.busy {
            return;
        }
        self.mobile.account.busy = true;
        self.mobile.account.generation = self.mobile.account.generation.wrapping_add(1);
        let generation = self.mobile.account.generation;
        self.mobile.error = None;
        let call = Arc::clone(&self.mobile.call);
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    account_call(
                        &call,
                        if logout {
                            "relay_auth_logout"
                        } else {
                            "relay_auth_start"
                        },
                    )
                })
                .await;
            this.update(cx, |this, cx| {
                if this.mobile.account.generation != generation {
                    return;
                }
                this.mobile.account.busy = false;
                if let Ok(data) = result {
                    this.mobile.accept_account(&data);
                    if let Some(url) = data
                        .get("verification_uri_complete")
                        .and_then(serde_json::Value::as_str)
                    {
                        this.mobile.account.pending = true;
                        cx.open_url(url);
                    }
                    if data.get("revoked").and_then(serde_json::Value::as_bool) == Some(false) {
                        this.mobile.error = Some(text::ACCOUNT_REVOKE_FAILED.get().to_owned());
                    }
                } else {
                    this.mobile.account.connect_after_login = None;
                    this.mobile.error = Some(text::ACCOUNT_ERROR.get().to_owned());
                }
                this.mobile_resume_after_login(cx);
                this.mobile_account_poll(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }
    /// 계정 한 줄. 로그인이 필요 없는 경로에서는 안내만, 릴레이에서는 상태와 그때 쓸 버튼만 보인다.
    pub(super) fn mobile_account_line(&self, cx: &mut Context<Self>) -> gpui::Stateful<gpui::Div> {
        let p = crate::ui::palette(cx);
        let account = &self.mobile.account;
        let row = div()
            .id("mobile-relay-account")
            .debug_selector(|| "mobile-relay-account".into());
        if self.mobile.mode != ConnectMode::Tunnel && !account.pending {
            return row
                .text_size(crate::ui::TEXT_XS)
                .text_color(p.muted_foreground)
                .child(text::ACCOUNT_LOCAL.get());
        }
        let logout = || {
            layout::ghost("mobile-relay-logout", text::ACCOUNT_LOGOUT.get())
                .disabled(account.busy)
                .debug_selector(|| "mobile-relay-logout".into())
                .on_click(cx.listener(|this, _, _, cx| this.mobile_account_action(true, cx)))
        };
        let (label, action) = if let Some(email) = &account.email {
            (email.clone(), Some(logout()))
        } else if account.pending {
            (text::ACCOUNT_WAITING.get().to_owned(), Some(logout()))
        } else if !account.required {
            (text::ACCOUNT_OPTIONAL.get().to_owned(), None)
        } else {
            (
                text::ACCOUNT_REQUIRED.get().to_owned(),
                Some(
                    layout::primary("mobile-relay-login", text::ACCOUNT_LOGIN.get())
                        .disabled(account.busy)
                        .debug_selector(|| "mobile-relay-login".into())
                        .on_click(
                            cx.listener(|this, _, _, cx| this.mobile_account_action(false, cx)),
                        ),
                ),
            )
        };
        row.w_full()
            .min_w_0()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_1p5()
            .px_2()
            .py_1p5()
            .rounded(crate::ui::RADIUS_SM)
            .bg(p.background)
            .border_1()
            .border_color(p.border)
            .text_size(crate::ui::TEXT_XS)
            .child(div().flex_1().min_w_0().child(label))
            .children(action)
    }
}
