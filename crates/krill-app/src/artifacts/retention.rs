//! Default publishing period and per-link retention controls.
use super::*;
use gpui_component::menu::{DropdownMenu as _, PopupMenuItem};
use krill_proto::artifacts as schema;

pub(super) type RetentionCall = Arc<dyn Fn(String, u16) -> Result<Item, String> + Send + Sync>;

pub(super) fn cli_retention_call(state: Option<PathBuf>) -> RetentionCall {
    Arc::new(move |slug, days| {
        let exe = krill_proto::daemon::sibling_exe("krill").map_err(|e| e.to_string())?;
        retention_command(&mut Command::new(exe), state.as_deref(), &slug, days)
    })
}

pub(super) fn retention_command(
    command: &mut Command,
    state: Option<&Path>,
    slug: &str,
    days: u16,
) -> Result<Item, String> {
    let period = if days == 0 {
        "never".into()
    } else {
        days.to_string()
    };
    command.args([
        "artifacts",
        "retention",
        slug,
        "--expires-in",
        &period,
        "--json",
    ]);
    if let Some(dir) = state {
        command.env("KRILL_STATE_DIR", dir);
    }
    let output = command
        .stdin(Stdio::null())
        .output()
        .map_err(|e| e.to_string())?;
    if let Ok(error) = serde_json::from_slice::<schema::ErrorOutput>(&output.stdout) {
        return Err(error.error.message);
    }
    if !output.status.success() {
        return Err(format!(
            "krill artifacts retention: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    serde_json::from_slice(&output.stdout).map_err(|e| e.to_string())
}

pub(super) fn expiration_label(artifact: &Artifact) -> String {
    if artifact.retention_days == Some(0) {
        text::RETENTION_NEVER.get().into()
    } else {
        format_timestamp_short(artifact.expires_at.as_deref())
    }
}

impl View {
    pub(super) fn set_default_retention(&mut self, days: u16, cx: &mut Context<Self>) {
        if let Some(dir) = &self.state_dir
            && let Err(error) = schema::set_default_retention_days(dir, days)
        {
            self.error = Some(error.to_string());
            cx.notify();
            return;
        }
        self.default_retention_days = days;
        self.error = None;
        cx.notify();
    }

    pub(super) fn set_retention(&mut self, slug: String, days: u16, cx: &mut Context<Self>) {
        if self.loading
            || self.retention_slug.is_some()
            || self.deleting_slug.is_some()
            || !self.sharing_enabled
        {
            return;
        }
        let call = self.retention_call.clone();
        self.retention_slug = Some(slug.clone());
        self.error = None;
        self.retention_task = Some(cx.spawn(async move |this, cx| {
            let target = slug.clone();
            let result = cx
                .background_executor()
                .spawn(async move { call(target, days) })
                .await;
            this.update(cx, |this, cx| {
                this.retention_slug = None;
                match result {
                    Ok(item) => {
                        if let Some(current) = this
                            .items
                            .iter_mut()
                            .find(|item| item.artifact.slug == slug)
                        {
                            *current = item;
                        }
                    }
                    Err(error) => this.error = Some(error),
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    pub(super) fn render_retention_control(
        &self,
        item: Option<&Item>,
        p: &ui::Palette,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let selected = item.map_or(self.default_retention_days, |item| {
            item.artifact
                .retention_days
                .unwrap_or(schema::DEFAULT_RETENTION_DAYS)
        });
        let slug = item.map(|item| item.artifact.slug.clone());
        let disabled = item.is_some()
            && (!self.sharing_enabled
                || self.loading
                || self.retention_slug.is_some()
                || self.deleting_slug.is_some());
        let scope = if item.is_some() {
            "artifact-retention"
        } else {
            "artifacts-default-retention"
        };
        let options = [
            (7, text::RETENTION_7.get()),
            (30, text::RETENTION_30.get()),
            (90, text::RETENTION_90.get()),
            (365, text::RETENTION_365.get()),
            (0, text::RETENTION_NEVER.get()),
        ];
        let label = options
            .iter()
            .find(|(days, _)| *days == selected)
            .map_or_else(
                || text::RETENTION_DAYS.format(&[("days", &selected.to_string())]),
                |(_, label)| (*label).to_owned(),
            );
        let weak = cx.entity().downgrade();
        let dropdown = ui::secondary(scope, label)
            .w(px(120.))
            .dropdown_caret(true)
            .disabled(disabled)
            .debug_selector(move || scope.into())
            .dropdown_menu(move |mut menu, _, _| {
                for (days, label) in options {
                    let weak = weak.clone();
                    let slug = slug.clone();
                    menu = menu.item(
                        PopupMenuItem::new(label)
                            .checked(selected == days)
                            .on_click(move |_, _, cx| {
                                weak.update(cx, |this, cx| {
                                    if let Some(slug) = &slug {
                                        this.set_retention(slug.clone(), days, cx);
                                    } else {
                                        this.set_default_retention(days, cx);
                                    }
                                })
                                .ok();
                            }),
                    );
                }
                menu
            });
        div()
            .flex()
            .flex_col()
            .min_w_0()
            .gap_1()
            .p_3()
            .child(ui::field_label(
                if item.is_some() {
                    text::RETENTION_TITLE.get()
                } else {
                    text::RETENTION_DEFAULT.get()
                },
                None,
                p,
            ))
            .child(dropdown)
            .child(
                div()
                    .text_size(ui::TEXT_XS)
                    .text_color(p.muted_foreground)
                    .child(if item.is_some() {
                        text::RETENTION_HINT.get()
                    } else {
                        text::RETENTION_DEFAULT_HINT.get()
                    }),
            )
    }
}
