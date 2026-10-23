//! `Settings > 아티팩트` 페이지: 공개 공유 허용 스위치와 공유한 아티팩트 목록(링크 복사·열기·삭제).
//!
//! 공유 엔진은 앱에 두지 않는다(OWE-744). 스테이징된 `krill artifacts … --json`을 하위 프로세스로 실행해
//! `krill_proto::artifacts`에 고정된 스키마를 파싱·표시만 한다. 공개 게이트 파일만 앱이 직접 켜고 끈다
//! (CLI에는 켜는 명령이 없다). 정책 게이트일 뿐 보안 경계는 아니다 — 상태 디렉터리에 쓸 수 있는 프로세스는 우회할 수 있다.
//! - 목록: `krill artifacts list [--cursor <c>] --json` → `ListPage`
//! - 삭제: `krill artifacts delete <slug> --json`
//! - 공유(파일 머리 버튼): `krill artifacts share <file> --json` → `ShareOutcome`

use std::ffi::OsString;
use std::path::Path;

use gpui::{App, ClipboardItem, Context, FontWeight, SharedString, div, prelude::*, px};
use gpui_component::{Disableable as _, Sizable as _};
use gpui_kit_assets::IconName;
use krill_proto::artifacts::{
    ErrorOutput, ListEntry, ListPage, SHARING_DISABLED_CODE, ShareOutcome,
};

use super::space::CliRunner;
use super::{SettingsView, layout, state_dir};
use crate::i18n::catalog::artifacts as text;
use crate::ui::BadgeTone;

/// CLI 실패. 공개 게이트 거부는 따로 구분해 설정 페이지로 안내한다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CliFailure {
    SharingDisabled,
    Message(String),
}

impl CliFailure {
    /// 사용자에게 보일 한 줄.
    #[must_use]
    pub fn message(&self) -> String {
        match self {
            Self::SharingDisabled => text::DISABLED.get().to_owned(),
            Self::Message(m) => text::ERROR.format(&[("error", m)]),
        }
    }
}

/// `krill artifacts <args> --json`을 실행해 성공 JSON을 `T`로, 오류 봉투를 [`CliFailure`]로 바꾼다.
fn call<T: serde::de::DeserializeOwned>(run: &CliRunner, args: &[&str]) -> Result<T, CliFailure> {
    let mut argv: Vec<OsString> = vec!["artifacts".into()];
    argv.extend(args.iter().map(OsString::from));
    argv.push("--json".into());
    let out = run
        .raw(&argv)
        .map_err(|e| CliFailure::Message(e.to_string()))?;
    if let Ok(ErrorOutput { error }) = serde_json::from_slice::<ErrorOutput>(&out) {
        return Err(if error.code == SHARING_DISABLED_CODE {
            CliFailure::SharingDisabled
        } else {
            CliFailure::Message(error.message)
        });
    }
    serde_json::from_slice(&out).map_err(|e| CliFailure::Message(e.to_string()))
}

fn path_arg(path: &Path) -> Result<&str, CliFailure> {
    path.to_str()
        .ok_or_else(|| CliFailure::Message(format!("{}: path must be UTF-8", path.display())))
}

/// 파일을 공유한다(기록이 있으면 같은 링크의 내용을 갱신한다).
///
/// # Errors
/// 게이트 거부·CLI 실패·네트워크 오류.
pub fn share_path(run: &CliRunner, path: &Path) -> Result<ShareOutcome, CliFailure> {
    call(run, &["share", path_arg(path)?])
}

fn list_page(run: &CliRunner, cursor: Option<&str>) -> Result<ListPage, CliFailure> {
    match cursor {
        Some(c) => call(run, &["list", "--cursor", c]),
        None => call(run, &["list"]),
    }
}

fn delete_slug(run: &CliRunner, slug: &str) -> Result<(), CliFailure> {
    call::<serde_json::Value>(run, &["delete", slug]).map(|_| ())
}

/// 스테이징된 `krill` CLI 실행기. 바이너리를 못 찾으면 `None`.
#[must_use]
pub fn runner(cx: &App) -> Option<CliRunner> {
    crate::orca_import::resolve_krill_bin(cx).map(|bin| CliRunner::process(bin, state_dir(cx)))
}

/// 이 기기에서 공개 공유가 허용됐는가(게이트 파일).
#[must_use]
pub fn sharing_enabled(cx: &App) -> bool {
    state_dir(cx).is_some_and(|d| krill_proto::artifacts::sharing_enabled(&d))
}

/// `SettingsView`가 소유하는 아티팩트 페이지 상태.
#[derive(Debug, Default)]
pub struct Panel {
    generation: u64,
    /// 테스트용 CLI 실행기 오버라이드. `None`이면 스테이징된 `krill` 바이너리를 실행한다.
    pub runner_override: Option<CliRunner>,
    pub items: Vec<ListEntry>,
    pub next_cursor: Option<String>,
    /// 한 번이라도 목록을 받았는가(빈 목록과 미조회를 구분).
    pub loaded: bool,
    pub loading: bool,
    /// 삭제 중인 slug.
    pub deleting: Option<String>,
    pub error: Option<SharedString>,
    pub notice: Option<SharedString>,
}

impl SettingsView {
    /// 테스트에서 CLI 실행기를 주입하고 목록을 다시 받는다.
    pub fn set_artifacts_runner(&mut self, runner: CliRunner, cx: &mut Context<Self>) {
        self.artifacts.runner_override = Some(runner);
        self.refresh_artifacts(cx);
    }

    fn artifacts_runner(&mut self, cx: &mut Context<Self>) -> Option<CliRunner> {
        if let Some(run) = self.artifacts.runner_override.clone() {
            return Some(run);
        }
        let run = runner(cx);
        if run.is_none() {
            self.artifacts.error = Some(
                text::ERROR
                    .format(&[(
                        "error",
                        crate::i18n::catalog::orca_import::ERR_CLI_MISSING.get(),
                    )])
                    .into(),
            );
            cx.notify();
        }
        run
    }

    /// 탭 진입 시 한 번만 자동 조회한다.
    pub(super) fn artifacts_enter(&mut self, cx: &mut Context<Self>) {
        if !self.artifacts.loaded && !self.artifacts.loading {
            self.refresh_artifacts(cx);
        }
    }

    /// 첫 페이지부터 다시 받는다.
    pub fn refresh_artifacts(&mut self, cx: &mut Context<Self>) {
        self.artifacts.generation = self.artifacts.generation.wrapping_add(1);
        self.artifacts.loading = false;
        self.load_artifacts(None, cx);
    }

    fn load_artifacts(&mut self, cursor: Option<String>, cx: &mut Context<Self>) {
        if self.artifacts.loading {
            return;
        }
        self.artifacts.error = None;
        let Some(run) = self.artifacts_runner(cx) else {
            return;
        };
        self.artifacts.loading = true;
        let generation = self.artifacts.generation;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let append = cursor.is_some();
            let res = cx
                .background_executor()
                .spawn(async move { list_page(&run, cursor.as_deref()) })
                .await;
            this.update(cx, |this, cx| {
                if this.artifacts.generation != generation {
                    return;
                }
                this.artifacts.loading = false;
                match res {
                    Ok(page) => {
                        if !append {
                            this.artifacts.items.clear();
                        }
                        this.artifacts.items.extend(page.artifacts);
                        this.artifacts.next_cursor = page.next_cursor;
                        this.artifacts.loaded = true;
                    }
                    Err(e) => this.artifacts.error = Some(e.message().into()),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn load_more_artifacts(&mut self, cx: &mut Context<Self>) {
        if let Some(cursor) = self.artifacts.next_cursor.clone() {
            self.load_artifacts(Some(cursor), cx);
        }
    }

    /// 서버에서 아티팩트를 지우고 목록에서 뺀다.
    pub fn delete_artifact(&mut self, slug: String, cx: &mut Context<Self>) {
        if self.artifacts.deleting.is_some() {
            return;
        }
        let Some(run) = self.artifacts_runner(cx) else {
            return;
        };
        self.artifacts.error = None;
        self.artifacts.notice = None;
        self.artifacts.deleting = Some(slug.clone());
        cx.notify();
        cx.spawn(async move |this, cx| {
            let target = slug.clone();
            let res = cx
                .background_executor()
                .spawn(async move { delete_slug(&run, &target) })
                .await;
            this.update(cx, |this, cx| {
                this.artifacts.deleting = None;
                match res {
                    Ok(()) => {
                        let name = this
                            .artifacts
                            .items
                            .iter()
                            .find(|e| e.artifact.slug == slug)
                            .map_or_else(|| slug.clone(), |e| e.artifact.display_name().to_owned());
                        this.artifacts.items.retain(|e| e.artifact.slug != slug);
                        this.artifacts.notice =
                            Some(text::DELETED.format(&[("name", &name)]).into());
                    }
                    Err(e) => this.artifacts.error = Some(e.message().into()),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// 공개 공유 허용을 켜고 끈다(게이트 파일).
    pub fn set_artifact_sharing(&mut self, enabled: bool, cx: &mut Context<Self>) {
        let Some(dir) = state_dir(cx) else {
            self.artifacts.error = Some(
                text::ERROR
                    .format(&[("error", "cannot resolve state dir")])
                    .into(),
            );
            cx.notify();
            return;
        };
        let res = if enabled {
            krill_proto::artifacts::enable_sharing(&dir)
        } else {
            krill_proto::artifacts::disable_sharing(&dir)
        };
        if let Err(e) = res {
            self.artifacts.error = Some(text::ERROR.format(&[("error", &e.to_string())]).into());
        }
        cx.notify();
    }

    fn copy_artifact_link(&mut self, url: &str, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(url.to_owned()));
        self.artifacts.notice = Some(text::COPIED.format(&[("url", url)]).into());
        cx.notify();
    }

    pub(super) fn render_artifacts_tab(&self, cx: &mut Context<Self>) -> gpui::Div {
        let p = crate::ui::palette(cx);
        let enabled = sharing_enabled(cx);
        let view = cx.entity();
        let allow_row = layout::row(p)
            .child(layout::label(
                p,
                text::ALLOW_TITLE.get(),
                Some(text::ALLOW_DESC.get()),
            ))
            .child(layout::controls().child(layout::switch(
                p,
                "admin-artifacts-sharing",
                enabled,
                false,
                move |active, _, cx| {
                    view.update(cx, |this, cx| this.set_artifact_sharing(active, cx));
                },
            )));
        let howto = layout::card(p)
            .id("admin-artifacts-howto")
            .debug_selector(|| "admin-artifacts-howto".into())
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .text_size(crate::ui::TEXT_SM)
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(text::HOWTO_TITLE.get()),
            )
            .child(
                div()
                    .text_size(crate::ui::TEXT_XS)
                    .text_color(p.muted_foreground)
                    .child(text::HOWTO_BODY.get()),
            );

        let header = layout::row(p)
            .flex_wrap()
            .child(
                layout::label(p, text::LIST_TITLE.get(), Some(text::LIST_DESC.get()))
                    .min_w(px(200.)),
            )
            .child(
                layout::controls().child(
                    crate::ui::button_with_reserved_label(
                        "admin-artifacts-refresh",
                        text::REFRESH.get(),
                        text::LOADING.get(),
                        self.artifacts.loading,
                    )
                    .small()
                    .outline()
                    .icon(IconName::RefreshCw)
                    .disabled(self.artifacts.loading)
                    .debug_selector(|| "admin-artifacts-refresh".into())
                    .on_click(cx.listener(|this, _, _, cx| this.refresh_artifacts(cx))),
                ),
            );

        let mut root = div()
            .flex()
            .flex_col()
            .gap_3()
            .child(allow_row)
            .child(howto)
            .child(header);
        if let Some(err) = &self.artifacts.error {
            root = root.child(
                crate::ui::error_banner(p, err.clone())
                    .id("admin-artifacts-error")
                    .debug_selector(|| "admin-artifacts-error".into()),
            );
        }
        if let Some(notice) = &self.artifacts.notice {
            root = root.child(
                div()
                    .id("admin-artifacts-notice")
                    .debug_selector(|| "admin-artifacts-notice".into())
                    .text_size(crate::ui::TEXT_XS)
                    .text_color(p.muted_foreground)
                    .child(notice.clone()),
            );
        }
        if self.artifacts.items.is_empty() {
            // 조회가 실패했으면 오류 배너만 남긴다("불러오는 중"을 같이 띄우지 않는다).
            if self.artifacts.error.is_some() && !self.artifacts.loading {
                return root;
            }
            let msg = if self.artifacts.loading || !self.artifacts.loaded {
                text::LOADING.get()
            } else {
                text::EMPTY.get()
            };
            return root.child(
                layout::card(p)
                    .id("admin-artifacts-empty")
                    .debug_selector(|| "admin-artifacts-empty".into())
                    .text_size(crate::ui::TEXT_XS)
                    .text_color(p.muted_foreground)
                    .child(msg),
            );
        }
        let mut list = div()
            .id("admin-artifacts-list")
            .debug_selector(|| "admin-artifacts-list".into())
            .flex()
            .flex_col()
            .gap_2();
        for entry in &self.artifacts.items {
            list = list.child(self.render_artifact_row(entry, cx));
        }
        root = root.child(list);
        if self.artifacts.next_cursor.is_some() {
            root = root.child(
                div().child(
                    layout::secondary("admin-artifacts-more", text::LOAD_MORE.get())
                        .disabled(self.artifacts.loading)
                        .debug_selector(|| "admin-artifacts-more".into())
                        .on_click(cx.listener(|this, _, _, cx| this.load_more_artifacts(cx))),
                ),
            );
        }
        root
    }

    fn render_artifact_row(&self, entry: &ListEntry, cx: &mut Context<Self>) -> gpui::Div {
        let p = crate::ui::palette(cx);
        let a = &entry.artifact;
        let slug = a.slug.clone();
        let kind = if a.is_markdown() { "Markdown" } else { "HTML" };
        let mut meta = div()
            .flex()
            .items_center()
            .gap_1p5()
            .flex_wrap()
            .child(crate::ui::badge(p, BadgeTone::Muted, kind));
        if let Some(exp) = a.expires_at.as_deref().filter(|s| !s.is_empty()) {
            let date = exp.split('T').next().unwrap_or(exp);
            meta = meta.child(
                div()
                    .text_size(crate::ui::TEXT_2XS)
                    .text_color(p.muted_foreground)
                    .child(text::EXPIRES.format(&[("date", date)])),
            );
        }
        let url = entry.share_url.clone();
        let copy_url = url.clone();
        let busy = self.artifacts.deleting.as_deref() == Some(slug.as_str());
        let del_slug = slug.clone();
        layout::row(p)
            .debug_selector({
                let s = slug.clone();
                move || format!("admin-artifact-row-{s}")
            })
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .truncate()
                            .text_size(crate::ui::TEXT_SM)
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(a.display_name().to_owned()),
                    )
                    .child(
                        div()
                            .truncate()
                            .font_family(crate::ui::FONT_MONO)
                            .text_size(crate::ui::TEXT_2XS)
                            .text_color(p.muted_foreground)
                            .child(url.clone()),
                    )
                    .child(meta),
            )
            .child(
                layout::controls()
                    .child(
                        layout::secondary(
                            SharedString::from(format!("admin-artifact-copy-{slug}")),
                            text::COPY_LINK.get(),
                        )
                        .icon(IconName::Copy)
                        .disabled(url.is_empty())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.copy_artifact_link(&copy_url, cx);
                        })),
                    )
                    .child(
                        layout::secondary(
                            SharedString::from(format!("admin-artifact-open-{slug}")),
                            text::OPEN.get(),
                        )
                        .icon(IconName::ExternalLink)
                        .disabled(url.is_empty())
                        .on_click(move |_, _, cx| cx.open_url(&url)),
                    )
                    .child(
                        self.confirm_button(
                            format!("admin-artifact-delete-{slug}"),
                            text::DELETE.get(),
                            cx,
                            move |this, cx| this.delete_artifact(del_slug.clone(), cx),
                        )
                        .disabled(busy || self.artifacts.deleting.is_some()),
                    ),
            )
    }
}
