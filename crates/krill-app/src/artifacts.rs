//! Artifact library. Fetch only on open, refresh, and pagination; no watcher or polling.

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Arc;

use gpui::{
    ClipboardItem, Context, Entity, EventEmitter, FocusHandle, Subscription, Task, Window, div,
    prelude::*,
};
use gpui_component::input::{Input, InputState};
use gpui_component::{Disableable as _, IconName};
use serde::Deserialize;

use crate::i18n::catalog::artifacts as text;
use crate::ui;

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Artifact {
    slug: String,
    title: Option<String>,
    original_file_name: Option<String>,
    expires_at: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Item {
    artifact: Artifact,
    share_url: String,
}

impl Item {
    fn title(&self) -> &str {
        self.artifact
            .title
            .as_deref()
            .filter(|s| !s.is_empty())
            .or(self.artifact.original_file_name.as_deref())
            .unwrap_or(&self.artifact.slug)
    }

    fn matches(&self, query: &str) -> bool {
        let haystack = format!(
            "{} {} {}",
            self.title(),
            self.artifact
                .original_file_name
                .as_deref()
                .unwrap_or_default(),
            self.artifact.slug,
        )
        .to_lowercase();
        query.split_whitespace().all(|term| haystack.contains(term))
    }

    fn openable(&self) -> bool {
        self.share_url.starts_with("https://") || self.share_url.starts_with("http://")
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Page {
    artifacts: Vec<Item>,
    next_cursor: Option<String>,
}

pub type Call = Arc<dyn Fn(Option<String>) -> Result<Page, String> + Send + Sync>;

fn cli_call(state: Option<PathBuf>) -> Call {
    Arc::new(move |cursor| {
        let exe = krill_proto::daemon::sibling_exe("krill").map_err(|e| e.to_string())?;
        list_command(&mut Command::new(exe), state.as_deref(), cursor)
    })
}

fn list_command(
    command: &mut Command,
    state: Option<&std::path::Path>,
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

#[derive(Debug)]
pub struct Close;

/// Always available, including before the first artifact is published.
pub struct View {
    focus: FocusHandle,
    input: Option<Entity<InputState>>,
    input_sub: Option<Subscription>,
    query: String,
    items: Vec<Item>,
    next: Option<String>,
    error: Option<String>,
    loading: bool,
    slow_load: bool,
    call: Call,
    task: Option<Task<()>>,
    loading_feedback: Option<Task<()>>,
    focus_search: bool,
    language: crate::i18n::Language,
}

impl EventEmitter<Close> for View {}

impl std::fmt::Debug for View {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Artifacts")
            .field("items", &self.items.len())
            .field("loading", &self.loading)
            .finish_non_exhaustive()
    }
}

impl View {
    pub fn new(state: Option<PathBuf>, cx: &mut Context<Self>) -> Self {
        Self::with_call(cli_call(state), cx)
    }

    pub fn with_call(call: Call, cx: &mut Context<Self>) -> Self {
        let mut view = Self {
            focus: cx.focus_handle(),
            input: None,
            input_sub: None,
            query: String::new(),
            items: Vec::new(),
            next: None,
            error: None,
            loading: false,
            slow_load: false,
            call,
            task: None,
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

    fn load(&mut self, more: bool, cx: &mut Context<Self>) {
        if self.loading || (more && self.next.is_none()) {
            return;
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
        let mut rows = div()
            .id("artifact-list")
            .debug_selector(|| "artifact-list".into())
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .flex()
            .flex_col();
        let mut visible = 0;
        for (index, item) in self
            .items
            .iter()
            .enumerate()
            .filter(|(_, i)| i.matches(&self.query))
        {
            visible += 1;
            let open = item.share_url.clone();
            let copy = open.clone();
            let id = format!("artifact-row-{index}");
            let selector = id.clone();
            rows = rows.child(
                div()
                    .id(id)
                    .debug_selector(move || selector.clone())
                    .w_full()
                    .flex()
                    .items_center()
                    .gap_3()
                    .px_3()
                    .py_2()
                    .border_b_1()
                    .border_color(p.border)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .text_size(ui::TEXT_SM)
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
                            .text_size(ui::TEXT_XS)
                            .text_color(p.muted_foreground)
                            .child(
                                item.artifact
                                    .expires_at
                                    .clone()
                                    .unwrap_or_else(|| "—".into()),
                            ),
                    )
                    .child(
                        ui::ghost(format!("artifact-copy-{index}"), text::COPY.get())
                            .debug_selector(move || format!("artifact-copy-{index}"))
                            .disabled(!item.openable())
                            .on_click(move |_, _, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(copy.clone()));
                            }),
                    )
                    .child(
                        ui::secondary(format!("artifact-open-{index}"), text::OPEN.get())
                            .debug_selector(move || format!("artifact-open-{index}"))
                            .disabled(!item.openable())
                            .on_click(move |_, _, cx| cx.open_url(&open)),
                    ),
            );
        }
        if visible == 0 && !self.loading && self.error.is_none() {
            rows = rows.child(
                div()
                    .id("artifacts-empty")
                    .p_6()
                    .text_size(ui::TEXT_SM)
                    .text_color(p.muted_foreground)
                    .child(if self.items.is_empty() {
                        text::EMPTY.get()
                    } else {
                        text::NO_MATCHES.get()
                    }),
            );
        }
        div()
            .id("artifacts-page")
            .debug_selector(|| "artifacts-page".into())
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|_, event: &gpui::KeyDownEvent, _, cx| {
                if event.keystroke.key == "escape" && !event.keystroke.modifiers.modified() {
                    cx.emit(Close);
                    cx.stop_propagation();
                }
            }))
            .size_full()
            .flex()
            .flex_col()
            .gap_3()
            .p_4()
            .bg(p.background)
            .font_family(ui::FONT_UI)
            .text_size(ui::TEXT_MD)
            .text_color(p.foreground)
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
                            .flex_1()
                            .text_size(ui::TEXT_LG)
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child(text::TITLE.get()),
                    )
                    .child(
                        ui::secondary("artifacts-refresh", text::REFRESH.get())
                            .debug_selector(|| "artifacts-refresh".into())
                            .disabled(self.loading)
                            .on_click(cx.listener(|this, _, _, cx| this.load(false, cx))),
                    ),
            )
            .child(
                div()
                    .text_size(ui::TEXT_SM)
                    .text_color(p.muted_foreground)
                    .child(text::DESCRIPTION.get()),
            )
            .children(self.input.as_ref().map(|input| {
                div()
                    .id("artifacts-search")
                    .debug_selector(|| "artifacts-search".into())
                    .child(Input::new(input))
            }))
            .children(self.error.as_ref().map(|e| ui::error_banner(p, e.clone())))
            .when(self.slow_load, |d| {
                d.child(div().id("artifacts-loading").child(text::LOADING.get()))
            })
            .child(rows)
            .when(self.next.is_some(), |d| {
                d.child(
                    ui::secondary("artifacts-more", text::MORE.get())
                        .debug_selector(|| "artifacts-more".into())
                        .disabled(self.loading)
                        .on_click(cx.listener(|this, _, _, cx| this.load(true, cx))),
                )
            })
    }
}

#[cfg(test)]
mod tests;

mod sharing;
pub use sharing::{runner, share_path};
