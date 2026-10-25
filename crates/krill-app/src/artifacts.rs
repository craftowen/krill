//! Artifact library (Orca `ArtifactsPage` UI/UX parity).
//! Fetch only on open, refresh, and pagination; no watcher or polling.
//! UI/design adapted from Orca (src/renderer/src/components/artifacts/ArtifactsPage.tsx).
//! Copyright (c) 2026 Lovecast Inc., MIT License; implemented here in Rust.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;

use gpui::{
    ClipboardItem, Context, Entity, EventEmitter, FocusHandle, FontWeight, KeyDownEvent,
    SharedString, Subscription, Task, Window, div, prelude::*, px, relative,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::{Input, InputState};
use gpui_component::{Disableable as _, IconName, Sizable as _};
use serde::Deserialize;

use crate::i18n::catalog::artifacts as text;
use crate::ui;

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Artifact {
    pub slug: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub original_file_name: Option<String>,
    #[serde(default)]
    pub source_content_type: Option<String>,
    #[serde(default)]
    pub updated_at: Option<String>,
    #[serde(default)]
    pub expires_at: Option<String>,
    #[serde(default)]
    pub byte_size: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Item {
    pub artifact: Artifact,
    pub share_url: String,
}

impl Item {
    #[must_use]
    pub fn title(&self) -> &str {
        self.artifact
            .title
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .or_else(|| {
                self.artifact
                    .original_file_name
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
            })
            .unwrap_or(&self.artifact.slug)
    }

    #[must_use]
    pub fn is_markdown(&self) -> bool {
        if self
            .artifact
            .source_content_type
            .as_deref()
            .is_some_and(|ct| ct.trim().to_ascii_lowercase().starts_with("text/markdown"))
        {
            return true;
        }
        let ext = Path::new(
            self.artifact
                .original_file_name
                .as_deref()
                .or(self.artifact.title.as_deref())
                .unwrap_or_default()
                .trim(),
        )
        .extension()
        .and_then(|e| e.to_str());
        ext.is_some_and(|e| e.eq_ignore_ascii_case("md") || e.eq_ignore_ascii_case("markdown"))
    }

    #[must_use]
    pub fn type_label(&self) -> &'static str {
        if self.is_markdown() {
            "Markdown"
        } else {
            "HTML"
        }
    }

    #[must_use]
    pub fn matches(&self, query: &str) -> bool {
        let haystack = format!(
            "{} {} {} {}",
            self.title(),
            self.artifact
                .original_file_name
                .as_deref()
                .unwrap_or_default(),
            self.artifact.slug,
            self.type_label(),
        )
        .to_lowercase();
        query.split_whitespace().all(|term| haystack.contains(term))
    }

    #[must_use]
    pub fn openable(&self) -> bool {
        self.share_url.starts_with("https://") || self.share_url.starts_with("http://")
    }
}

/// Format byte sizes in Orca's compact table style (`450 B`, `1.2 KB`, `14 KB`, `2.4 MB`).
#[must_use]
pub fn format_byte_size(byte_size: Option<u64>) -> String {
    let Some(bytes) = byte_size else {
        return "—".into();
    };
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    if bytes < 1024 * 1024 {
        if bytes >= 10 * 1024 {
            let kb = (bytes + 512) / 1024;
            return format!("{kb} KB");
        }
        let tenths = (bytes * 10 + 512) / 1024;
        return format!("{}.{} KB", tenths / 10, tenths % 10);
    }
    let tenths = (bytes * 10 + 512 * 1024) / (1024 * 1024);
    format!("{}.{} MB", tenths / 10, tenths % 10)
}

/// Format ISO-8601 or date strings into compact table timestamps (`YYYY-MM-DD HH:MM` or `YYYY-MM-DD`).
#[must_use]
pub fn format_timestamp_short(value: Option<&str>) -> String {
    let Some(raw) = value.map(str::trim).filter(|s| !s.is_empty()) else {
        return "—".into();
    };
    if let Some((date, time_part)) = raw.split_once('T') {
        let hm: String = time_part.chars().take(5).collect();
        if hm.len() == 5 && hm.as_bytes().get(2) == Some(&b':') {
            return format!("{date} {hm}");
        }
        return date.to_owned();
    }
    raw.to_owned()
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Page {
    pub artifacts: Vec<Item>,
    pub next_cursor: Option<String>,
}

pub type Call = Arc<dyn Fn(Option<String>) -> Result<Page, String> + Send + Sync>;
pub type DeleteCall = Arc<dyn Fn(String) -> Result<(), String> + Send + Sync>;

fn cli_call(state: Option<PathBuf>) -> Call {
    Arc::new(move |cursor| {
        let exe = krill_proto::daemon::sibling_exe("krill").map_err(|e| e.to_string())?;
        list_command(&mut Command::new(exe), state.as_deref(), cursor)
    })
}

fn cli_delete_call(state: Option<PathBuf>) -> DeleteCall {
    Arc::new(move |slug| {
        let exe = krill_proto::daemon::sibling_exe("krill").map_err(|e| e.to_string())?;
        delete_command(&mut Command::new(exe), state.as_deref(), &slug)
    })
}

fn list_command(
    command: &mut Command,
    state: Option<&Path>,
    cursor: Option<String>,
) -> Result<Page, String> {
    command.args(["artifacts", "list", "--json"]);
    if let Some(dir) = state {
        command.env("KRILL_STATE_DIR", dir);
    }
    if let Some(cursor) = cursor {
        command.arg("--cursor").arg(cursor);
    }
    let output = command
        .stdin(Stdio::null())
        .output()
        .map_err(|e| e.to_string())?;
    if !output.status.success() {
        let error = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        return Err(if error.is_empty() {
            format!("krill artifacts: {}", output.status)
        } else {
            error
        });
    }
    serde_json::from_slice(&output.stdout).map_err(|e| e.to_string())
}

fn delete_command(command: &mut Command, state: Option<&Path>, slug: &str) -> Result<(), String> {
    command.args(["artifacts", "delete", slug, "--json"]);
    if let Some(dir) = state {
        command.env("KRILL_STATE_DIR", dir);
    }
    let output = command
        .stdin(Stdio::null())
        .output()
        .map_err(|e| e.to_string())?;
    if !output.status.success() {
        let error = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        return Err(if error.is_empty() {
            format!("krill artifacts delete: {}", output.status)
        } else {
            error
        });
    }
    Ok(())
}

#[derive(Debug)]
pub struct Close;

/// Always available, including before the first artifact is published.
#[expect(clippy::struct_excessive_bools, reason = "UI view state flags")]
pub struct View {
    focus: FocusHandle,
    state_dir: Option<PathBuf>,
    sharing_enabled: bool,
    input: Option<Entity<InputState>>,
    input_sub: Option<Subscription>,
    query: String,
    items: Vec<Item>,
    next: Option<String>,
    selected_slug: Option<String>,
    copied_slug: Option<String>,
    deleting_slug: Option<String>,
    error: Option<String>,
    loading: bool,
    slow_load: bool,
    call: Call,
    delete_call: DeleteCall,
    task: Option<Task<()>>,
    delete_task: Option<Task<()>>,
    loading_feedback: Option<Task<()>>,
    focus_search: bool,
    language: crate::i18n::Language,
}

impl EventEmitter<Close> for View {}

impl std::fmt::Debug for View {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Artifacts")
            .field("items", &self.items.len())
            .field("selected_slug", &self.selected_slug)
            .field("loading", &self.loading)
            .finish_non_exhaustive()
    }
}

impl View {
    pub fn new(state: Option<PathBuf>, cx: &mut Context<Self>) -> Self {
        Self::with_calls(
            cli_call(state.clone()),
            cli_delete_call(state.clone()),
            state,
            cx,
        )
    }

    pub fn with_call(call: Call, cx: &mut Context<Self>) -> Self {
        Self::with_calls(call, Arc::new(|_| Ok(())), None, cx)
    }

    pub fn with_calls(
        call: Call,
        delete_call: DeleteCall,
        state_dir: Option<PathBuf>,
        cx: &mut Context<Self>,
    ) -> Self {
        let sharing_enabled = state_dir
            .as_deref()
            .is_none_or(krill_proto::artifacts::sharing_enabled);
        let mut view = Self {
            focus: cx.focus_handle(),
            state_dir,
            sharing_enabled,
            input: None,
            input_sub: None,
            query: String::new(),
            items: Vec::new(),
            next: None,
            selected_slug: None,
            copied_slug: None,
            deleting_slug: None,
            error: None,
            loading: false,
            slow_load: false,
            call,
            delete_call,
            task: None,
            delete_task: None,
            loading_feedback: None,
            focus_search: true,
            language: crate::i18n::language(),
        };
        view.load(false, cx);
        view
    }

    pub fn focus_handle(&self) -> FocusHandle {
        self.focus.clone()
    }

    #[must_use]
    pub fn selected_slug(&self) -> Option<&str> {
        self.selected_slug.as_deref()
    }

    pub fn select_slug(&mut self, slug: Option<String>, cx: &mut Context<Self>) {
        self.selected_slug = slug;
        cx.notify();
    }

    pub fn clear_search(&mut self, window: Option<&mut Window>, cx: &mut Context<Self>) {
        self.query.clear();
        if let Some(input) = &self.input
            && let Some(window) = window
        {
            input.update(cx, |input, cx| {
                input.set_value(String::new(), window, cx);
            });
        }
        cx.notify();
    }

    pub fn handle_escape(&mut self, window: Option<&mut Window>, cx: &mut Context<Self>) {
        if !self.query.is_empty() {
            self.clear_search(window, cx);
            return;
        }
        if self.selected_slug.is_some() {
            self.selected_slug = None;
            cx.notify();
            return;
        }
        cx.emit(Close);
    }

    pub fn toggle_sharing(&mut self, enabled: bool, cx: &mut Context<Self>) {
        let Some(dir) = self.state_dir.clone() else {
            self.sharing_enabled = enabled;
            cx.notify();
            return;
        };
        let res = if enabled {
            krill_proto::artifacts::enable_sharing(&dir)
        } else {
            krill_proto::artifacts::disable_sharing(&dir)
        };
        match res {
            Ok(()) => {
                self.sharing_enabled = enabled;
                self.error = None;
            }
            Err(err) => {
                self.error = Some(err.to_string());
            }
        }
        cx.notify();
    }

    pub fn delete_artifact(&mut self, slug: String, cx: &mut Context<Self>) {
        if self.deleting_slug.is_some() {
            return;
        }
        let call = self.delete_call.clone();
        self.deleting_slug = Some(slug.clone());
        self.error = None;
        self.delete_task = Some(cx.spawn(async move |this, cx| {
            let slug_for_call = slug.clone();
            let result = cx
                .background_executor()
                .spawn(async move { call(slug_for_call) })
                .await;
            this.update(cx, |this, cx| {
                this.deleting_slug = None;
                match result {
                    Ok(()) => {
                        this.items.retain(|i| i.artifact.slug != slug);
                        if this.selected_slug.as_deref() == Some(slug.as_str()) {
                            this.selected_slug = None;
                        }
                    }
                    Err(err) => {
                        this.error = Some(err);
                    }
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn load(&mut self, more: bool, cx: &mut Context<Self>) {
        if self.loading || (more && self.next.is_none()) {
            return;
        }
        if let Some(dir) = &self.state_dir {
            self.sharing_enabled = krill_proto::artifacts::sharing_enabled(dir);
        }
        let cursor = more.then(|| self.next.clone()).flatten();
        let requested_cursor = cursor.clone();
        let call = self.call.clone();
        self.loading = true;
        self.slow_load = false;
        self.error = None;
        self.loading_feedback = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_secs(1))
                .await;
            this.update(cx, |this, cx| {
                this.slow_load = this.loading;
                cx.notify();
            })
            .ok();
        }));
        self.task = Some(cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { call(cursor) })
                .await;
            this.update(cx, |this, cx| {
                this.loading = false;
                this.slow_load = false;
                this.loading_feedback = None;
                match result {
                    Ok(page) => {
                        if !more {
                            this.items.clear();
                        }
                        for item in page.artifacts {
                            if let Some(existing) = this
                                .items
                                .iter_mut()
                                .find(|i| i.artifact.slug == item.artifact.slug)
                            {
                                *existing = item;
                            } else {
                                this.items.push(item);
                            }
                        }
                        if let Some(sel) = &this.selected_slug
                            && !this.items.iter().any(|i| &i.artifact.slug == sel)
                        {
                            this.selected_slug = None;
                        }
                        // A repeated cursor must not leave a misleading endless Load more button.
                        this.next = page
                            .next_cursor
                            .filter(|c| !c.is_empty() && requested_cursor.as_ref() != Some(c));
                    }
                    Err(error) => this.error = Some(error),
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn local_source_for_slug(&self, slug: &str) -> Option<(String, Option<String>)> {
        #[derive(Deserialize)]
        struct LocalShareEntry {
            slug: String,
        }
        #[derive(Default, Deserialize)]
        struct LocalSharesFile {
            #[serde(default)]
            files: std::collections::BTreeMap<String, LocalShareEntry>,
        }
        let dir = self.state_dir.as_deref()?;
        let raw = std::fs::read(dir.join("artifact-shares.json")).ok()?;
        let shares: LocalSharesFile = serde_json::from_slice(&raw).ok()?;
        let (path, _) = shares.files.into_iter().find(|(_, s)| s.slug == slug)?;
        let preview = std::fs::read_to_string(&path).ok().map(|content| {
            let lines: Vec<&str> = content.lines().take(28).collect();
            lines.join("\n")
        });
        Some((path, preview))
    }

    fn render_error_banner(&self, p: &ui::Palette, cx: &mut Context<Self>) -> Option<gpui::Div> {
        let error = self.error.as_ref()?;
        Some(
            div().px_5().pb_3().child(
                div()
                    .id("artifacts-error-banner")
                    .debug_selector(|| "artifacts-error-banner".into())
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_4()
                    .rounded(ui::RADIUS_MD)
                    .border_1()
                    .border_color(p.destructive.opacity(0.35))
                    .bg(p.destructive.opacity(0.1))
                    .px_3()
                    .py_2()
                    .child(
                        div()
                            .flex_1()
                            .text_size(ui::TEXT_XS)
                            .text_color(p.destructive)
                            .child(error.clone()),
                    )
                    .child(
                        ui::secondary("artifacts-retry", text::RETRY.get())
                            .debug_selector(|| "artifacts-retry".into())
                            .disabled(self.loading)
                            .on_click(cx.listener(|this, _, _, cx| this.load(false, cx))),
                    ),
            ),
        )
    }

    fn render_skeleton(p: &ui::Palette) -> gpui::Stateful<gpui::Div> {
        let skel_row = |idx: usize| {
            div()
                .id(SharedString::from(format!("artifacts-skeleton-row-{idx}")))
                .h(px(40.))
                .w_full()
                .flex()
                .items_center()
                .gap_3()
                .px_3()
                .border_b_1()
                .border_color(p.border.opacity(0.6))
                .child(
                    div()
                        .flex_1()
                        .h(px(12.))
                        .rounded(ui::RADIUS_SM)
                        .bg(p.muted.opacity(0.5)),
                )
                .child(
                    div()
                        .w(px(72.))
                        .h(px(12.))
                        .rounded(ui::RADIUS_SM)
                        .bg(p.muted.opacity(0.4)),
                )
                .child(
                    div()
                        .w(px(56.))
                        .h(px(12.))
                        .rounded(ui::RADIUS_SM)
                        .bg(p.muted.opacity(0.4)),
                )
                .child(
                    div()
                        .w(px(88.))
                        .h(px(12.))
                        .rounded(ui::RADIUS_SM)
                        .bg(p.muted.opacity(0.4)),
                )
                .child(
                    div()
                        .w(px(88.))
                        .h(px(12.))
                        .rounded(ui::RADIUS_SM)
                        .bg(p.muted.opacity(0.4)),
                )
        };
        div()
            .id("artifacts-loading")
            .debug_selector(|| "artifacts-loading".into())
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .gap_3()
            .px_5()
            .pb_5()
            .child(
                div()
                    .text_size(ui::TEXT_XS)
                    .text_color(p.muted_foreground)
                    .child(text::LOADING.get()),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .rounded(ui::RADIUS_MD)
                    .border_1()
                    .border_color(p.border)
                    .bg(p.muted.opacity(0.2))
                    .overflow_hidden()
                    .flex()
                    .flex_col()
                    .child(Self::render_table_header(p))
                    .child(skel_row(0))
                    .child(skel_row(1))
                    .child(skel_row(2))
                    .child(skel_row(3)),
            )
    }

    fn render_empty_state(
        &self,
        p: &ui::Palette,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let publishing_blocked = !self.sharing_enabled;
        let has_more = self.next.is_some();

        let (heading, description) = if publishing_blocked {
            (
                text::PUBLISHING_OFF_HEADING.get(),
                text::PUBLISHING_OFF_DESC.get(),
            )
        } else if has_more {
            (text::MORE_HEADING.get(), text::MORE_DESC.get())
        } else {
            (text::EMPTY_HEADING.get(), text::EMPTY_DESC.get())
        };

        let mut container = div()
            .id("artifacts-empty")
            .debug_selector(|| "artifacts-empty".into())
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_3()
            .px_6()
            .text_center()
            .child(ui::icons::svg_box(
                ui::icons::FILES_SVG,
                32.0,
                p.muted_foreground,
            ))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap_1()
                    .child(
                        div()
                            .text_size(ui::TEXT_LG)
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(p.foreground)
                            .child(heading),
                    )
                    .child(
                        div()
                            .max_w(px(384.))
                            .text_size(ui::TEXT_XS)
                            .text_color(p.muted_foreground)
                            .child(description),
                    ),
            );

        if publishing_blocked {
            container = container.child(
                ui::secondary("artifacts-enable-sharing", text::ENABLE_SHARING.get())
                    .debug_selector(|| "artifacts-enable-sharing".into())
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_sharing(true, cx))),
            );
        } else if has_more {
            container = container.child(
                ui::secondary("artifacts-more", text::MORE.get())
                    .debug_selector(|| "artifacts-more".into())
                    .disabled(self.loading)
                    .on_click(cx.listener(|this, _, _, cx| this.load(true, cx))),
            );
        }

        container
    }

    fn render_table_header(p: &ui::Palette) -> gpui::Div {
        div()
            .h(px(32.))
            .w_full()
            .flex_none()
            .flex()
            .items_center()
            .gap_3()
            .px_3()
            .border_b_1()
            .border_color(p.border)
            .bg(p.muted.opacity(0.4))
            .text_size(ui::TEXT_XS)
            .font_weight(FontWeight::MEDIUM)
            .text_color(p.muted_foreground)
            .child(div().flex_1().min_w(px(160.)).child(text::COL_NAME.get()))
            .child(div().w(px(84.)).flex_none().child(text::COL_TYPE.get()))
            .child(div().w(px(72.)).flex_none().child(text::COL_SIZE.get()))
            .child(div().w(px(116.)).flex_none().child(text::COL_UPDATED.get()))
            .child(div().w(px(116.)).flex_none().child(text::COL_EXPIRES.get()))
            .child(
                div()
                    .w(px(184.))
                    .flex_none()
                    .flex()
                    .justify_end()
                    .child(text::COL_ACTIONS.get()),
            )
    }

    fn render_detail_drawer(
        &self,
        item: &Item,
        p: &ui::Palette,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let slug = item.artifact.slug.clone();
        let slug_for_copy = slug.clone();
        let slug_for_delete = slug.clone();
        let share_url = item.share_url.clone();
        let share_url_open = share_url.clone();
        let share_url_preview_open = share_url.clone();
        let is_openable = item.openable();
        let is_copied = self.copied_slug.as_deref() == Some(slug.as_str());
        let is_deleting = self.deleting_slug.as_deref() == Some(slug.as_str());
        let updated = format_timestamp_short(item.artifact.updated_at.as_deref());
        let size = format_byte_size(item.artifact.byte_size);
        let expires = format_timestamp_short(item.artifact.expires_at.as_deref());
        let meta_line = format!(
            "{} {updated} · {size} · {} {expires}",
            text::COL_UPDATED.get(),
            text::COL_EXPIRES.get()
        );
        let local_source = self.local_source_for_slug(&slug);

        div()
            .id("artifact-detail-drawer")
            .debug_selector(|| "artifact-detail-drawer".into())
            .w(px(440.))
            .max_w(relative(0.5))
            .h_full()
            .flex_none()
            .border_l_1()
            .border_color(p.border)
            .bg(p.background)
            .flex()
            .flex_col()
            .overflow_hidden()
            .child(
                // ArtifactDetailHeader
                div()
                    .flex_none()
                    .flex()
                    .flex_col()
                    .gap_2p5()
                    .px_4()
                    .py_3()
                    .border_b_1()
                    .border_color(p.border)
                    .child(
                        div()
                            .flex()
                            .items_start()
                            .justify_between()
                            .gap_2()
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .flex()
                                    .flex_col()
                                    .gap_0p5()
                                    .child(
                                        div()
                                            .text_size(ui::TEXT_MD)
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .text_color(p.foreground)
                                            .truncate()
                                            .child(item.title().to_owned()),
                                    )
                                    .child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .gap_1()
                                            .text_size(ui::TEXT_XS)
                                            .text_color(p.muted_foreground)
                                            .child(ui::icons::svg_box(
                                                ui::icons::GLOBE_SVG,
                                                ui::icons::ICON_XS,
                                                p.muted_foreground,
                                            ))
                                            .child(
                                                div()
                                                    .flex_1()
                                                    .min_w_0()
                                                    .font_family(ui::FONT_MONO)
                                                    .truncate()
                                                    .child(item.share_url.clone()),
                                            ),
                                    )
                                    .child(
                                        div()
                                            .text_size(ui::TEXT_XS)
                                            .text_color(p.muted_foreground)
                                            .truncate()
                                            .child(meta_line),
                                    ),
                            )
                            .child(
                                ui::ghost("artifact-detail-close", "")
                                    .debug_selector(|| "artifact-detail-close".into())
                                    .icon(IconName::Close)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.selected_slug = None;
                                        cx.notify();
                                    })),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .items_center()
                            .gap_1p5()
                            .child(
                                ui::primary(
                                    "artifact-detail-copy",
                                    if is_copied {
                                        crate::i18n::link_ui::COPIED_LINK.get()
                                    } else {
                                        text::COPY_LINK.get()
                                    },
                                )
                                .debug_selector(|| "artifact-detail-copy".into())
                                .disabled(!is_openable)
                                .on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        cx.write_to_clipboard(ClipboardItem::new_string(
                                            share_url.clone(),
                                        ));
                                        this.copied_slug = Some(slug_for_copy.clone());
                                        cx.notify();
                                    },
                                )),
                            )
                            .child(
                                ui::secondary("artifact-detail-open", text::OPEN_IN_BROWSER.get())
                                    .debug_selector(|| "artifact-detail-open".into())
                                    .disabled(!is_openable)
                                    .on_click(move |_, _, cx| cx.open_url(&share_url_open)),
                            )
                            .child(
                                Button::new("artifact-detail-delete")
                                    .small()
                                    .danger()
                                    .label(text::DELETE.get())
                                    .debug_selector(|| "artifact-detail-delete".into())
                                    .disabled(is_deleting)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.delete_artifact(slug_for_delete.clone(), cx);
                                    })),
                            ),
                    ),
            )
            .child(
                // ArtifactPreview surface
                div()
                    .id("artifact-detail-preview-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .bg(p.editor_surface)
                    .p_4()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(
                        div()
                            .rounded(ui::RADIUS_MD)
                            .border_1()
                            .border_color(p.border)
                            .bg(p.background)
                            .p_3()
                            .flex()
                            .flex_col()
                            .gap_2()
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .justify_between()
                                    .child(
                                        div()
                                            .text_size(ui::TEXT_SM)
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .text_color(p.foreground)
                                            .child(text::PREVIEW_TITLE.get()),
                                    )
                                    .child(ui::badge(p, ui::BadgeTone::Neutral, item.type_label())),
                            )
                            .child(
                                div()
                                    .text_size(ui::TEXT_XS)
                                    .text_color(p.muted_foreground)
                                    .child(text::PREVIEW_SUBTITLE.get()),
                            )
                            .child(
                                div()
                                    .pt_1()
                                    .flex()
                                    .flex_col()
                                    .gap_1()
                                    .text_size(ui::TEXT_XS)
                                    .child(
                                        div()
                                            .flex()
                                            .justify_between()
                                            .gap_2()
                                            .child(
                                                div()
                                                    .text_color(p.muted_foreground)
                                                    .child(text::SLUG_LABEL.get()),
                                            )
                                            .child(
                                                div()
                                                    .font_family(ui::FONT_MONO)
                                                    .text_color(p.foreground)
                                                    .child(item.artifact.slug.clone()),
                                            ),
                                    )
                                    .children(item.artifact.original_file_name.as_ref().map(
                                        |orig| {
                                            div()
                                                .flex()
                                                .justify_between()
                                                .gap_2()
                                                .child(
                                                    div()
                                                        .text_color(p.muted_foreground)
                                                        .child(text::COL_NAME.get()),
                                                )
                                                .child(
                                                    div()
                                                        .font_family(ui::FONT_MONO)
                                                        .text_color(p.foreground)
                                                        .truncate()
                                                        .child(orig.clone()),
                                                )
                                        },
                                    )),
                            )
                            .child(
                                div().pt_1().child(
                                    ui::secondary(
                                        "artifact-preview-open-browser",
                                        text::OPEN_IN_BROWSER.get(),
                                    )
                                    .disabled(!is_openable)
                                    .on_click(move |_, _, cx| cx.open_url(&share_url_preview_open)),
                                ),
                            ),
                    )
                    .children(local_source.map(|(source_path, preview_text)| {
                        div()
                            .rounded(ui::RADIUS_MD)
                            .border_1()
                            .border_color(p.border)
                            .bg(p.background)
                            .p_3()
                            .flex()
                            .flex_col()
                            .gap_2()
                            .child(
                                div()
                                    .text_size(ui::TEXT_XS)
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(p.muted_foreground)
                                    .child(text::LOCAL_SOURCE.get()),
                            )
                            .child(
                                div()
                                    .font_family(ui::FONT_MONO)
                                    .text_size(ui::TEXT_XS)
                                    .text_color(p.foreground)
                                    .truncate()
                                    .child(source_path),
                            )
                            .children(preview_text.filter(|s| !s.trim().is_empty()).map(
                                |snippet| {
                                    div()
                                        .rounded(ui::RADIUS_SM)
                                        .border_1()
                                        .border_color(p.border)
                                        .bg(p.muted.opacity(0.25))
                                        .p_2()
                                        .font_family(ui::FONT_MONO)
                                        .text_size(ui::TEXT_XS)
                                        .text_color(p.foreground)
                                        .child(snippet)
                                },
                            ))
                    })),
            )
    }
}

impl Render for View {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = ui::palette(cx);
        if self.input.is_none() && cx.has_global::<gpui_component::Theme>() {
            let input = cx.new(|cx| InputState::new(window, cx).placeholder(text::SEARCH.get()));
            self.input_sub = Some(cx.observe(&input, |this, input, cx| {
                this.query = input.read(cx).value().to_lowercase();
                cx.notify();
            }));
            self.input = Some(input);
        }
        let language_changed = self.language != crate::i18n::language();
        self.language = crate::i18n::language();
        if let Some(input) = &self.input {
            if language_changed || self.focus_search {
                input.update(cx, |input, cx| {
                    if language_changed {
                        input.set_placeholder(text::SEARCH.get(), window, cx);
                    }
                    if self.focus_search {
                        input.focus(window, cx);
                    }
                });
            }
            self.focus_search = false;
        }

        let selected_item = self
            .selected_slug
            .as_deref()
            .and_then(|slug| self.items.iter().find(|i| i.artifact.slug == slug))
            .cloned();

        let mut rows = div()
            .id("artifact-list")
            .debug_selector(|| "artifact-list".into())
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .flex()
            .flex_col();

        let mut visible = 0usize;
        for (index, item) in self
            .items
            .iter()
            .enumerate()
            .filter(|(_, i)| i.matches(&self.query))
        {
            visible += 1;
            let open = item.share_url.clone();
            let copy = open.clone();
            let slug = item.artifact.slug.clone();
            let slug_select = slug.clone();
            let slug_copy = slug.clone();
            let slug_delete = slug.clone();
            let is_selected = self.selected_slug.as_deref() == Some(slug.as_str());
            let is_deleting = self.deleting_slug.as_deref() == Some(slug.as_str());
            let id = format!("artifact-row-{index}");
            let selector = id.clone();
            let type_label = item.type_label();
            let size_label = format_byte_size(item.artifact.byte_size);
            let updated_label = format_timestamp_short(item.artifact.updated_at.as_deref());
            let expires_label = format_timestamp_short(item.artifact.expires_at.as_deref());

            rows = rows.child(
                div()
                    .id(id)
                    .debug_selector(move || selector.clone())
                    .w_full()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap_3()
                    .px_3()
                    .py_2()
                    .border_b_1()
                    .border_color(p.border)
                    .cursor_pointer()
                    .when(is_selected, |d| d.bg(p.accent.opacity(0.6)))
                    .hover(|s| {
                        s.bg(if is_selected {
                            p.accent.opacity(0.6)
                        } else {
                            p.muted.opacity(0.25)
                        })
                    })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.select_slug(Some(slug_select.clone()), cx);
                    }))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(160.))
                            .flex()
                            .flex_col()
                            .gap_0p5()
                            .child(
                                div()
                                    .text_size(ui::TEXT_SM)
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(p.foreground)
                                    .truncate()
                                    .child(item.title().to_owned()),
                            )
                            .child(
                                div()
                                    .text_size(ui::TEXT_XS)
                                    .text_color(p.muted_foreground)
                                    .truncate()
                                    .child(item.share_url.clone()),
                            ),
                    )
                    .child(
                        div()
                            .w(px(84.))
                            .flex_none()
                            .text_size(ui::TEXT_XS)
                            .text_color(p.muted_foreground)
                            .truncate()
                            .child(type_label),
                    )
                    .child(
                        div()
                            .w(px(72.))
                            .flex_none()
                            .text_size(ui::TEXT_XS)
                            .text_color(p.muted_foreground)
                            .truncate()
                            .child(size_label),
                    )
                    .child(
                        div()
                            .w(px(116.))
                            .flex_none()
                            .text_size(ui::TEXT_XS)
                            .text_color(p.muted_foreground)
                            .truncate()
                            .child(updated_label),
                    )
                    .child(
                        div()
                            .w(px(116.))
                            .flex_none()
                            .text_size(ui::TEXT_XS)
                            .text_color(p.muted_foreground)
                            .truncate()
                            .child(expires_label),
                    )
                    .child(
                        div()
                            .w(px(184.))
                            .flex_none()
                            .flex()
                            .items_center()
                            .justify_end()
                            .gap_1()
                            .child(
                                ui::ghost(format!("artifact-copy-{index}"), text::COPY.get())
                                    .debug_selector(move || format!("artifact-copy-{index}"))
                                    .disabled(!item.openable())
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        cx.stop_propagation();
                                        cx.write_to_clipboard(ClipboardItem::new_string(
                                            copy.clone(),
                                        ));
                                        this.copied_slug = Some(slug_copy.clone());
                                        cx.notify();
                                    })),
                            )
                            .child(
                                ui::secondary(format!("artifact-open-{index}"), text::OPEN.get())
                                    .debug_selector(move || format!("artifact-open-{index}"))
                                    .disabled(!item.openable())
                                    .on_click(move |_, _, cx| {
                                        cx.stop_propagation();
                                        cx.open_url(&open);
                                    }),
                            )
                            .child(
                                ui::ghost(format!("artifact-delete-{index}"), "")
                                    .debug_selector(move || format!("artifact-delete-{index}"))
                                    .icon(IconName::Delete)
                                    .disabled(is_deleting)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        cx.stop_propagation();
                                        this.delete_artifact(slug_delete.clone(), cx);
                                    })),
                            ),
                    ),
            );
        }

        if visible == 0 && !self.items.is_empty() && !self.loading && self.error.is_none() {
            rows = rows.child(
                div()
                    .id("artifacts-empty")
                    .debug_selector(|| "artifacts-empty".into())
                    .px_3()
                    .py_6()
                    .text_center()
                    .text_size(ui::TEXT_XS)
                    .text_color(p.muted_foreground)
                    .child(text::NO_MATCHES.get()),
            );
        }

        // Toolbar (ArtifactListToolbar)
        let toolbar = div()
            .flex_none()
            .flex()
            .flex_wrap()
            .items_center()
            .justify_between()
            .gap_2()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .children(self.input.as_ref().map(|input| {
                        div()
                            .id("artifacts-search")
                            .debug_selector(|| "artifacts-search".into())
                            .w(px(224.))
                            .flex()
                            .items_center()
                            .gap_1()
                            .child(div().flex_1().child(Input::new(input).small()))
                            .when(!self.query.is_empty(), |d| {
                                d.child(
                                    ui::ghost("artifacts-search-clear", "")
                                        .debug_selector(|| "artifacts-search-clear".into())
                                        .icon(IconName::Close)
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.clear_search(Some(window), cx);
                                        })),
                                )
                            })
                    }))
                    .child(
                        ui::secondary("artifacts-refresh", text::REFRESH.get())
                            .debug_selector(|| "artifacts-refresh".into())
                            .disabled(self.loading)
                            .on_click(cx.listener(|this, _, _, cx| this.load(false, cx))),
                    ),
            )
            .when(self.state_dir.is_some(), |d| {
                let enabled = self.sharing_enabled;
                d.child(
                    ui::ghost(
                        "artifacts-sharing-toggle",
                        if enabled {
                            text::SHARING_ON.get()
                        } else {
                            text::SHARING_OFF.get()
                        },
                    )
                    .debug_selector(|| "artifacts-sharing-toggle".into())
                    .on_click(cx.listener(move |this, _, _, cx| this.toggle_sharing(!enabled, cx))),
                )
            });

        let main_body = if self.slow_load && self.items.is_empty() {
            Self::render_skeleton(p).into_any_element()
        } else if self.items.is_empty() && !self.loading && self.error.is_none() {
            div()
                .id("artifact-list")
                .debug_selector(|| "artifact-list".into())
                .flex_1()
                .min_h_0()
                .flex()
                .flex_col()
                .gap_3()
                .px_5()
                .pb_5()
                .child(toolbar)
                .child(self.render_empty_state(p, cx))
                .into_any_element()
        } else {
            div()
                .flex_1()
                .min_h_0()
                .flex()
                .flex_col()
                .gap_3()
                .px_5()
                .pb_5()
                .child(toolbar)
                .when(self.slow_load, |d| {
                    d.child(
                        div()
                            .id("artifacts-loading")
                            .debug_selector(|| "artifacts-loading".into())
                            .text_size(ui::TEXT_XS)
                            .text_color(p.muted_foreground)
                            .child(text::LOADING.get()),
                    )
                })
                .child(
                    div()
                        .flex_1()
                        .min_h_0()
                        .flex()
                        .flex_col()
                        .overflow_hidden()
                        .rounded(ui::RADIUS_MD)
                        .border_1()
                        .border_color(p.border)
                        .bg(p.muted.opacity(0.2))
                        .child(Self::render_table_header(p))
                        .child(rows),
                )
                .when(self.next.is_some(), |d| {
                    d.child(
                        div().flex().justify_center().child(
                            ui::secondary("artifacts-more", text::MORE.get())
                                .debug_selector(|| "artifacts-more".into())
                                .disabled(self.loading)
                                .on_click(cx.listener(|this, _, _, cx| this.load(true, cx))),
                        ),
                    )
                })
                .into_any_element()
        };

        div()
            .id("artifacts-page")
            .debug_selector(|| "artifacts-page".into())
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                if event.keystroke.key == "escape" && !event.keystroke.modifiers.modified() {
                    this.handle_escape(Some(window), cx);
                    cx.stop_propagation();
                }
            }))
            .size_full()
            .flex()
            .bg(p.background)
            .font_family(ui::FONT_UI)
            .text_size(ui::TEXT_MD)
            .text_color(p.foreground)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .flex()
                    .flex_col()
                    .child(
                        // Top Header (Orca ArtifactsPage header + Krill Back button)
                        div()
                            .flex_none()
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap_4()
                            .px_5()
                            .pt_5()
                            .pb_3()
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .child(
                                        ui::ghost("artifacts-back", text::BACK.get())
                                            .debug_selector(|| "artifacts-back".into())
                                            .icon(IconName::ArrowLeft)
                                            .on_click(cx.listener(|_, _, _, cx| cx.emit(Close))),
                                    )
                                    .child(
                                        div()
                                            .text_size(ui::TEXT_LG)
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .text_color(p.foreground)
                                            .child(text::TITLE.get()),
                                    ),
                            )
                            .child(
                                div()
                                    .text_size(ui::TEXT_XS)
                                    .text_color(p.muted_foreground)
                                    .truncate()
                                    .child(text::DESCRIPTION.get()),
                            ),
                    )
                    .children(self.render_error_banner(p, cx))
                    .child(main_body),
            )
            .children(
                selected_item
                    .as_ref()
                    .map(|item| self.render_detail_drawer(item, p, cx)),
            )
    }
}

#[cfg(test)]
mod tests;

mod sharing;
pub use sharing::{runner, share_path};
