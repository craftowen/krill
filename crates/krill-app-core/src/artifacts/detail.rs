//! Full-width artifact detail view with local-file access and in-app preview.

use std::collections::BTreeMap;
use std::io::Read as _;
use std::time::{Duration, Instant};

use super::*;

const MAX_PREVIEW_BYTES: usize = 256 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum DetailTab {
    Preview,
    Source,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct DetailContent {
    pub local_path: Option<String>,
    pub local_exists: bool,
    pub text: Option<String>,
    pub truncated: bool,
    pub loading_remote: bool,
}

#[derive(Clone, Debug)]
pub(super) struct DetailState {
    pub slug: String,
    pub tab: DetailTab,
    pub content: DetailContent,
}

fn truncate_utf8(bytes: &[u8]) -> (String, bool) {
    let truncated = bytes.len() > MAX_PREVIEW_BYTES;
    let slice = if truncated {
        &bytes[..MAX_PREVIEW_BYTES]
    } else {
        bytes
    };
    (String::from_utf8_lossy(slice).into_owned(), truncated)
}

fn parse_local_share_path(raw: &[u8], slug: &str) -> Option<String> {
    #[derive(Deserialize)]
    struct LocalShareEntry {
        slug: String,
    }
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum SharesDoc {
        Wrapped {
            files: BTreeMap<String, LocalShareEntry>,
        },
        Flat(BTreeMap<String, LocalShareEntry>),
    }

    let entries = match serde_json::from_slice::<SharesDoc>(raw).ok()? {
        SharesDoc::Wrapped { files } | SharesDoc::Flat(files) => files,
    };
    entries
        .into_iter()
        .find(|(_, entry)| entry.slug == slug)
        .map(|(path, _)| path)
}

/// Look up the local source file for `slug` from `artifact-shares.json` (flat CLI map or legacy
/// wrapped format) and read up to [`MAX_PREVIEW_BYTES`] on a background thread.
pub(super) fn local_source_for_slug(
    dir: &Path,
    slug: &str,
) -> Option<(String, bool, Option<String>, bool)> {
    let raw = std::fs::read(dir.join("artifact-shares.json")).ok()?;
    let path = parse_local_share_path(&raw, slug)?;
    let exists = Path::new(&path).is_file();
    let mut buf = Vec::new();
    let read_ok = std::fs::File::open(&path)
        .and_then(|file| {
            file.take((MAX_PREVIEW_BYTES + 1) as u64)
                .read_to_end(&mut buf)
        })
        .is_ok();
    if read_ok {
        let (text, truncated) = truncate_utf8(&buf);
        Some((path, exists, Some(text), truncated))
    } else {
        Some((path, exists, None, false))
    }
}

struct BoundedChild {
    child: std::process::Child,
    exited: bool,
}

impl Drop for BoundedChild {
    fn drop(&mut self) {
        if !self.exited {
            krill_proto::os::kill_tree(&mut self.child);
        }
    }
}

pub(super) fn read_bounded_command_preview(
    cmd: &mut std::process::Command,
    timeout: Duration,
) -> Option<(String, bool)> {
    let mut child = BoundedChild {
        child: cmd
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?,
        exited: false,
    };
    let stdout = child.child.stdout.take()?;
    let deadline = Instant::now() + timeout;
    let limit = (MAX_PREVIEW_BYTES + 1) as u64;
    let reader = crate::process_io::read(stdout, limit, deadline).ok()?;
    let mut captured_bytes: Option<Vec<u8>> = None;
    let status = loop {
        if captured_bytes.is_none()
            && let Some(res) = reader.try_finish()
        {
            let captured = res.ok()?;
            if captured.timed_out {
                return None;
            }
            if captured.bytes.len() > MAX_PREVIEW_BYTES {
                // We already received > MAX_PREVIEW_BYTES of a 2xx body (`curl --fail`
                // suppresses error bodies); stop reading and reap curl on drop.
                let (text, truncated) = truncate_utf8(&captured.bytes);
                return (!text.is_empty()).then_some((text, truncated));
            }
            captured_bytes = Some(captured.bytes);
        }
        if let Some(status) = child.child.try_wait().ok()? {
            child.exited = true;
            break status;
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    let bytes = if let Some(bytes) = captured_bytes {
        bytes
    } else {
        let captured = reader.finish(deadline + Duration::from_millis(50)).ok()?;
        if captured.timed_out {
            return None;
        }
        captured.bytes
    };
    if bytes.len() > MAX_PREVIEW_BYTES {
        // The child may have exited non-zero (SIGPIPE / curl exit 23) right after
        // the bounded reader closed stdout upon hitting `limit`, before `try_finish()`
        // was polled.
        let (text, truncated) = truncate_utf8(&bytes);
        return (!text.is_empty()).then_some((text, truncated));
    }
    if !status.success() {
        return None;
    }
    let (text, truncated) = truncate_utf8(&bytes);
    (!text.is_empty()).then_some((text, truncated))
}

pub(super) fn fetch_remote_raw(share_url: &str) -> Option<(String, bool)> {
    let clean = crate::terminal::path_link::sanitize_http_url(share_url)?;
    let raw_url = format!("{}/raw", clean.trim_end_matches('/'));
    let raw_url = crate::terminal::path_link::sanitize_http_url(&raw_url)?;
    let mut cmd = crate::platform::background_command("curl");
    cmd.args([
        "--fail",
        "--silent",
        "--show-error",
        "--location",
        "--globoff",
        "--proto",
        "=http,https",
        "--proto-redir",
        "=http,https",
        "--max-time",
        "8",
        &raw_url,
    ]);
    read_bounded_command_preview(&mut cmd, Duration::from_secs(10))
}

impl View {
    pub(super) fn set_detail_tab(&mut self, tab: DetailTab, cx: &mut Context<Self>) {
        if let Some(detail) = &mut self.detail
            && detail.tab != tab
        {
            detail.tab = tab;
            cx.notify();
        }
    }

    pub(super) fn render_detail_view(
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
        let share_url_fallback_open = share_url.clone();
        let is_openable = item.openable();
        let is_markdown = item.is_markdown();
        let is_copied = self.copied_slug.as_deref() == Some(slug.as_str());
        let is_deleting = self.deleting_slug.as_deref() == Some(slug.as_str());
        let updated = format_timestamp_short(item.artifact.updated_at.as_deref());
        let size = format_byte_size(item.artifact.byte_size);
        let expires = retention::expiration_label(&item.artifact);
        let meta_line = format!(
            "{} {updated} · {size} · {} {expires}",
            text::COL_UPDATED.get(),
            text::COL_EXPIRES.get()
        );

        let detail = self
            .detail
            .as_ref()
            .filter(|state| state.slug == slug)
            .cloned()
            .unwrap_or_else(|| DetailState {
                slug: slug.clone(),
                tab: if is_markdown {
                    DetailTab::Preview
                } else {
                    DetailTab::Source
                },
                content: DetailContent::default(),
            });

        let local_path_for_open = detail
            .content
            .local_exists
            .then(|| detail.content.local_path.clone())
            .flatten();

        let header = div()
            .flex_none()
            .flex()
            .flex_col()
            .gap_2p5()
            .px_5()
            .py_3()
            .border_b_1()
            .border_color(p.border)
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .min_w_0()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(ui::button_with_tooltip(
                                Button::new("artifact-detail-close")
                                    .small()
                                    .ghost()
                                    .debug_selector(|| "artifact-detail-close".into())
                                    .icon(IconName::ChevronLeft)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.select_slug(None, cx);
                                    })),
                                "artifact-detail-close",
                                text::BACK_TO_LIST.get(),
                            ))
                            .child(
                                div()
                                    .flex_none()
                                    .text_size(ui::TEXT_SM)
                                    .text_color(p.muted_foreground)
                                    .child(text::TITLE.get()),
                            )
                            .child(
                                div()
                                    .flex_none()
                                    .text_size(ui::TEXT_SM)
                                    .text_color(p.muted_foreground)
                                    .child("/"),
                            )
                            .child(
                                div()
                                    .min_w_0()
                                    .text_size(ui::TEXT_MD)
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(p.foreground)
                                    .truncate()
                                    .child(item.title().to_owned()),
                            )
                            .child(ui::badge(p, ui::BadgeTone::Neutral, item.type_label())),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .max_w(px(300.))
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
                                    .min_w_0()
                                    .font_family(ui::FONT_MONO)
                                    .truncate()
                                    .child(item.share_url.clone()),
                            ),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .min_w_0()
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .items_center()
                            .gap_1p5()
                            .min_w_0()
                            .child(
                                ui::button_with_reserved_label(
                                    "artifact-detail-copy",
                                    text::COPY_LINK.get(),
                                    crate::i18n::link_ui::COPIED_LINK.get(),
                                    is_copied,
                                )
                                .small()
                                .primary()
                                .debug_selector(|| "artifact-detail-copy".into())
                                .disabled(!is_openable)
                                .on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        this.armed_delete = None;
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
                                    .on_click(move |_, _, cx| {
                                        if let Some(clean) =
                                            crate::terminal::path_link::sanitize_http_url(
                                                &share_url_open,
                                            )
                                        {
                                            cx.open_url(&clean);
                                        }
                                    }),
                            )
                            .children(local_path_for_open.map(|local_path| {
                                ui::secondary("artifact-detail-open-local", text::OPEN_LOCAL.get())
                                    .debug_selector(|| "artifact-detail-open-local".into())
                                    .on_click(move |_, _, _| {
                                        crate::file_format::open_default_command(Path::new(
                                            &local_path,
                                        ))
                                        .spawn()
                                        .ok();
                                    })
                            }))
                            .child(self.render_inline_retention_control(Some(item), p, cx))
                            .child({
                                let detail_delete_armed = self.armed_delete.as_ref()
                                    == Some(&ArmedDelete::Detail(slug_for_delete.clone()));
                                let out_slug = slug_for_delete.clone();
                                ui::danger_confirm(
                                    "artifact-detail-delete",
                                    text::DELETE.get(),
                                    crate::i18n::catalog::settings::CONFIRM_AGAIN.get(),
                                    detail_delete_armed,
                                )
                                .debug_selector(|| "artifact-detail-delete".into())
                                .disabled(is_deleting || self.retention_slug.is_some())
                                .when(detail_delete_armed, |btn| {
                                    btn.on_mouse_down_out(cx.listener(move |this, _, _, cx| {
                                        if this.armed_delete.as_ref()
                                            == Some(&ArmedDelete::Detail(out_slug.clone()))
                                        {
                                            this.armed_delete = None;
                                            cx.notify();
                                        }
                                    }))
                                })
                                .on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        this.activate_detail_delete(slug_for_delete.clone(), cx);
                                    },
                                ))
                            }),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .text_size(ui::TEXT_XS)
                            .text_color(p.muted_foreground)
                            .truncate()
                            .child(meta_line),
                    ),
            );

        let local_strip = detail.content.local_path.clone().map(|source_path| {
            div()
                .debug_selector(|| "artifact-local-source".into())
                .rounded(ui::RADIUS_MD)
                .border_1()
                .border_color(p.border)
                .bg(p.background)
                .px_3()
                .py_2()
                .flex()
                .items_center()
                .justify_between()
                .gap_3()
                .min_w_0()
                .child(
                    div()
                        .flex_none()
                        .text_size(ui::TEXT_XS)
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(p.muted_foreground)
                        .child(text::LOCAL_SOURCE.get()),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .font_family(ui::FONT_MONO)
                        .text_size(ui::TEXT_XS)
                        .text_color(p.foreground)
                        .truncate()
                        .child(source_path),
                )
        });

        let preview_tabs = (is_markdown && detail.content.text.is_some()).then(|| {
            ui::segmented_control(
                p,
                [
                    ui::segment(
                        p,
                        "artifact-tab-preview",
                        text::PREVIEW_RENDERED.get(),
                        detail.tab == DetailTab::Preview,
                    )
                    .debug_selector(|| "artifact-tab-preview".into())
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.set_detail_tab(DetailTab::Preview, cx);
                    })),
                    ui::segment(
                        p,
                        "artifact-tab-source",
                        text::PREVIEW_SOURCE.get(),
                        detail.tab == DetailTab::Source,
                    )
                    .debug_selector(|| "artifact-tab-source".into())
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.set_detail_tab(DetailTab::Source, cx);
                    })),
                ],
            )
        });
        let truncated_badge = detail
            .content
            .truncated
            .then(|| ui::badge(p, ui::BadgeTone::Muted, "256 KB"));

        let preview_body = if let Some(content) = detail.content.text {
            if is_markdown && detail.tab == DetailTab::Preview {
                div()
                    .min_w_0()
                    .child(crate::file_format::view::markdown(
                        SharedString::from(format!("artifact-markdown-preview-{slug}")),
                        content,
                        cx,
                    ))
                    .into_any_element()
            } else {
                div()
                    .min_w_0()
                    .rounded(ui::RADIUS_SM)
                    .border_1()
                    .border_color(p.border)
                    .bg(p.muted.opacity(0.25))
                    .p_3()
                    .font_family(ui::FONT_MONO)
                    .text_size(ui::TEXT_XS)
                    .text_color(p.foreground)
                    .child(content)
                    .into_any_element()
            }
        } else if detail.content.loading_remote {
            div()
                .text_size(ui::TEXT_XS)
                .text_color(p.muted_foreground)
                .child(text::PREVIEW_LOADING.get())
                .into_any_element()
        } else {
            div()
                .flex()
                .flex_col()
                .gap_2()
                .child(
                    div()
                        .text_size(ui::TEXT_XS)
                        .text_color(p.muted_foreground)
                        .child(text::PREVIEW_SUBTITLE.get()),
                )
                .child(
                    div()
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
                                        .flex_none()
                                        .text_color(p.muted_foreground)
                                        .child(text::SLUG_LABEL.get()),
                                )
                                .child(
                                    div()
                                        .min_w_0()
                                        .truncate()
                                        .font_family(ui::FONT_MONO)
                                        .text_color(p.foreground)
                                        .child(item.artifact.slug.clone()),
                                ),
                        )
                        .children(item.artifact.original_file_name.as_ref().map(|orig| {
                            div()
                                .flex()
                                .justify_between()
                                .gap_2()
                                .child(
                                    div()
                                        .flex_none()
                                        .text_color(p.muted_foreground)
                                        .child(text::COL_NAME.get()),
                                )
                                .child(
                                    div()
                                        .min_w_0()
                                        .font_family(ui::FONT_MONO)
                                        .text_color(p.foreground)
                                        .truncate()
                                        .child(orig.clone()),
                                )
                        })),
                )
                .child(
                    div().pt_1().child(
                        ui::secondary("artifact-preview-open-browser", text::OPEN_IN_BROWSER.get())
                            .disabled(!is_openable)
                            .on_click(move |_, _, cx| {
                                if let Some(clean) = crate::terminal::path_link::sanitize_http_url(
                                    &share_url_fallback_open,
                                ) {
                                    cx.open_url(&clean);
                                }
                            }),
                    ),
                )
                .into_any_element()
        };

        let preview_card = div()
            .rounded(ui::RADIUS_MD)
            .border_1()
            .border_color(p.border)
            .bg(p.background)
            .p_4()
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .child(
                        div()
                            .text_size(ui::TEXT_SM)
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(p.foreground)
                            .child(text::PREVIEW_TITLE.get()),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .children(truncated_badge)
                            .children(preview_tabs),
                    ),
            )
            .child(preview_body);

        div()
            .id("artifact-detail-drawer")
            .debug_selector(|| "artifact-detail-drawer".into())
            .flex_1()
            .w_full()
            .min_w_0()
            .min_h_0()
            .bg(p.background)
            .flex()
            .flex_col()
            .overflow_hidden()
            .child(header)
            .child(
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
                    .children(local_strip)
                    .child(preview_card),
            )
    }
}
