//! 앱 안 피드백 보내기. 사용자가 쓴 글과 Krill 버전·OS·아키텍처만 `errors.krill.page/v1/feedback`으로 보낸다.

use std::io;
use std::time::Duration;

use gpui::{Context, Window, div, prelude::*};
use gpui_component::Disableable as _;
use gpui_component::input::{Textarea, TextareaState};

use super::{SettingsView, layout};
use crate::i18n::catalog::settings as text;

const ENDPOINT: &str = "https://errors.krill.page/v1/feedback";
/// 로컬 수신기 시험용 주소 덮어쓰기(`http://127.0.0.1:<port>/v1/feedback` 등).
const ENDPOINT_VAR: &str = "KRILL_FEEDBACK_ENDPOINT";
/// 수신기(`services/errors/src/feedback.js`)와 같은 글자 수 상한.
pub(super) const MAX_CHARS: usize = 4000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Status {
    Sending,
    Sent,
    Failed,
}

/// 보낼 글. 앞뒤 공백을 빼고 비었으면 `None`, 상한을 넘으면 잘라 낸다.
pub(super) fn message(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    (!trimmed.is_empty()).then(|| {
        trimmed
            .chars()
            .take(MAX_CHARS)
            .collect::<String>()
            .trim_end()
            .to_owned()
    })
}

fn body(message: &str) -> String {
    serde_json::json!({
        "message": message,
        "release": env!("CARGO_PKG_VERSION"),
        "os": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
    })
    .to_string()
}

/// 한 번 보낸다(백그라운드 스레드에서 부른다). 테스트 프로세스는 주소를 덮어쓰지 않으면 운영 수신기로 보내지 않는다.
pub(super) fn send(message: &str) -> io::Result<()> {
    let (endpoint, proto) = match std::env::var(ENDPOINT_VAR) {
        Ok(url) => (url, "=https,http"),
        Err(_) if krill_log::is_test_process() => {
            return Err(io::Error::other("feedback upload disabled in tests"));
        }
        Err(_) => (ENDPOINT.to_owned(), "=https"),
    };
    let body = body(message);
    let output = super::child::output(
        crate::platform::background_command(crate::updater::system_tool("curl")).args([
            "--disable",
            "--fail",
            "--silent",
            "--proto",
            proto,
            "--max-time",
            "20",
            "--header",
            "Content-Type: application/json",
            "--data-raw",
            &body,
            "--url",
            &endpoint,
        ]),
        Duration::from_secs(25),
        4096,
    )?;
    if output.status.success() {
        Ok(())
    } else {
        Err(io::Error::other("feedback upload failed"))
    }
}

impl SettingsView {
    pub(super) fn render_feedback_page(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let p = crate::ui::palette(cx);
        let input = self
            .feedback_input
            .get_or_insert_with(|| {
                cx.new(|cx| {
                    TextareaState::new(window, cx)
                        .auto_grow(6, 14)
                        .placeholder(text::FEEDBACK_PLACEHOLDER.get())
                })
            })
            .clone();
        let sending = self.feedback_status == Some(Status::Sending);
        let empty = message(&input.read(cx).value()).is_none();
        let status = self.feedback_status.and_then(|status| match status {
            Status::Sending => None,
            Status::Sent => Some((text::FEEDBACK_SENT.get(), p.muted_foreground)),
            Status::Failed => Some((text::FEEDBACK_FAILED.get(), p.destructive)),
        });
        div().child(
            layout::card(p)
                .flex()
                .flex_col()
                .gap_2()
                .child(
                    Textarea::new(&input)
                        .disabled(sending)
                        .aria_label(text::TAB_FEEDBACK.get()),
                )
                .child(
                    layout::controls()
                        .justify_between()
                        .child(
                            div()
                                .debug_selector(|| "admin-feedback-status".into())
                                .min_w_0()
                                .text_size(crate::ui::TEXT_XS)
                                .when_some(status, |d, (msg, color)| {
                                    d.text_color(color).child(msg)
                                }),
                        )
                        .child(
                            layout::primary("admin-feedback-send", text::FEEDBACK_SEND.get())
                                .debug_selector(|| "admin-feedback-send".into())
                                .loading(sending)
                                .disabled(sending || empty)
                                .on_click(cx.listener(|this, _, _, cx| this.send_feedback(cx))),
                        ),
                ),
        )
    }

    fn send_feedback(&mut self, cx: &mut Context<Self>) {
        let Some(input) = &self.feedback_input else {
            return;
        };
        let Some(msg) = message(&input.read(cx).value()) else {
            return;
        };
        if self.feedback_status == Some(Status::Sending) {
            return;
        }
        self.feedback_status = Some(Status::Sending);
        self.file_job(
            "feedback-send",
            move || send(&msg),
            |this, result, cx| {
                this.feedback_status = Some(if result.is_ok() {
                    // 보낸 글은 지운다. 다음에 그릴 때 빈 입력을 새로 만든다.
                    this.feedback_input = None;
                    Status::Sent
                } else {
                    Status::Failed
                });
                cx.notify();
            },
            cx,
        );
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_trims_rejects_blank_and_caps_length() {
        assert_eq!(message("  \n "), None);
        assert_eq!(
            message("  탭이 안 닫혀요\n").as_deref(),
            Some("탭이 안 닫혀요")
        );
        assert_eq!(
            message(&"가".repeat(MAX_CHARS + 5)).map(|m| m.chars().count()),
            Some(MAX_CHARS)
        );
    }

    #[test]
    fn body_carries_only_message_version_and_platform() {
        let value: serde_json::Value = serde_json::from_str(&body("@hi \"q\"")).unwrap();
        let object = value.as_object().unwrap();
        assert_eq!(object.len(), 4);
        assert_eq!(object["message"], "@hi \"q\"");
        assert_eq!(object["release"], env!("CARGO_PKG_VERSION"));
        assert!(["macos", "windows", "linux"].contains(&object["os"].as_str().unwrap()));
    }
}
