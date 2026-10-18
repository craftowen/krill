//! OWE-496 워크트리 생성 대화상자(MK-042 후속). 사이드바 레포 행의 "+"로 연다.
//! OWE-687 모양은 Orca `NewWorkspaceComposerModal`처럼 창 가운데 모달(라벨 붙은 입력칸, 오른쪽 아래 버튼).
//! IPC는 사이드바 [`Source`] 뒤에 있어 gpui 테스트는 인메모리 응답을 쓴다.
//! setup 스크립트는 내용과 해시를 보여 주고 사람이 승인한 해시만 돌려보낸다(krilld가 내용 변경 시 다시 묻는다).

use krill_proto::os::ffi::OsStrExt as _;
use ring::rand::{SecureRandom as _, SystemRandom};
use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use gpui::{Context, Entity, EventEmitter, Subscription, Task, Window, div, prelude::*, px};
use gpui_component::Sizable as _;
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::{Input, InputEvent, InputState};
use krill_proto::control::{
    NewWorktree, PrepareWorktree, Reply, Setup, Worktree, WorktreePreparation,
};

use crate::sidebar::Source;

use crate::i18n::catalog::worktree as text;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// 사이드바를 바로 새로 고친다. 대화상자는 결과(경고 포함)를 보여 주며 열려 있다.
    Created(Worktree),
    Close,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Phase {
    Edit(Option<String>),
    Creating,
    Untrusted { script: String, hash: Vec<u8> },
    Done(Worktree),
}

pub struct Dialog {
    language: crate::i18n::Language,
    repo: PathBuf,
    parent: Option<PathBuf>,
    name: Entity<InputState>,
    base: Entity<InputState>,
    phase: Phase,
    source: Arc<dyn Source>,
    task: Option<Task<()>>,
    _enter: [Subscription; 3],
    preparation: Option<Preparation>,
    preparation_task: Option<Task<()>>,
    preparation_ready: bool,
}

struct Preparation {
    req: PrepareWorktree,
    source: Arc<dyn Source>,
    cancelled: Arc<AtomicBool>,
    executor: gpui::BackgroundExecutor,
}

impl Drop for Preparation {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Release);
        let mut req = self.req.clone();
        req.cancel = true;
        let source = self.source.clone();
        // Entity release must never wait for IPC. The token also handles cancellation-before-start.
        self.executor
            .spawn(async move {
                source.prepare_worktree(req).ok();
            })
            .detach();
    }
}

impl EventEmitter<Event> for Dialog {}

impl std::fmt::Debug for Dialog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Dialog")
            .field("repo", &self.repo)
            .field("parent", &self.parent)
            .field("phase", &self.phase)
            .finish_non_exhaustive()
    }
}

/// 사람이 확인할 스크립트에서 보이지 않는 제어 문자(ESC 등)를 드러낸다. 줄바꿈·탭은 둔다.
fn visible(script: &str) -> String {
    script
        .chars()
        .map(|c| {
            if c.is_control() && c != '\n' && c != '\t' {
                char::REPLACEMENT_CHARACTER
            } else {
                c
            }
        })
        .collect()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut s, b| {
        write!(s, "{b:02x}").ok();
        s
    })
}

impl Dialog {
    pub fn new(
        repo: PathBuf,
        source: Arc<dyn Source>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::new_with_parent(repo, None, source, window, cx)
    }

    pub fn new_with_parent(
        repo: PathBuf,
        parent: Option<PathBuf>,
        source: Arc<dyn Source>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let name = cx.new(|cx| InputState::new(window, cx).placeholder(text::NAME.get()));
        let base = cx.new(|cx| InputState::new(window, cx).placeholder(text::BASE.get()));
        name.update(cx, |input, cx| input.focus(window, cx));
        let enter = |cx: &mut Context<Self>, input: &Entity<InputState>| {
            cx.subscribe(input, |d, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::PressEnter { .. })
                    && matches!(d.phase, Phase::Edit(_))
                {
                    d.submit(Setup::Ask, cx);
                }
            })
        };
        let subscriptions = [
            enter(cx, &name),
            enter(cx, &base),
            cx.subscribe(&base, |d, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    d.prepare(cx);
                }
            }),
        ];
        let mut dialog = Self {
            language: crate::i18n::language(),
            repo,
            parent,
            name,
            base,
            phase: Phase::Edit(None),
            source,
            task: None,
            _enter: subscriptions,
            preparation: None,
            preparation_task: None,
            preparation_ready: false,
        };
        dialog.prepare(cx);
        dialog
    }

    #[must_use]
    pub fn parent(&self) -> Option<&std::path::Path> {
        self.parent.as_deref()
    }

    fn cancel_preparation(&mut self, cx: &mut Context<Self>) {
        self.preparation = None;
        self.preparation_task = None;
        self.preparation_ready = false;
        cx.notify();
    }

    fn prepare(&mut self, cx: &mut Context<Self>) {
        self.cancel_preparation(cx);
        if !matches!(self.phase, Phase::Edit(_)) {
            return;
        }
        let mut token = [0; 8];
        if SystemRandom::new().fill(&mut token).is_err() {
            return;
        }
        let base = self.base.read(cx).value().trim().to_owned();
        let req = PrepareWorktree {
            token: u64::from_le_bytes(token),
            repo: self.repo.as_os_str().as_bytes().to_vec(),
            base: (!base.is_empty()).then_some(base),
            cancel: false,
        };
        let source = self.source.clone();
        let cancelled = Arc::new(AtomicBool::new(false));
        self.preparation = Some(Preparation {
            req: req.clone(),
            source: source.clone(),
            cancelled: cancelled.clone(),
            executor: cx.background_executor().clone(),
        });
        self.preparation_task = Some(cx.spawn(async move |this, cx| {
            // Base edits debounce; opening the dialog is the only automatic preparation trigger.
            cx.background_executor()
                .timer(Duration::from_millis(300))
                .await;
            loop {
                let (source, req, cancelled) = (source.clone(), req.clone(), cancelled.clone());
                let reply = cx
                    .background_executor()
                    .spawn(async move {
                        if cancelled.load(Ordering::Acquire) {
                            return None;
                        }
                        let result = source.prepare_worktree(req.clone());
                        if cancelled.load(Ordering::Acquire) {
                            let mut cancel = req;
                            cancel.cancel = true;
                            source.prepare_worktree(cancel).ok();
                            return None;
                        }
                        Some(result)
                    })
                    .await;
                match reply {
                    Some(Ok(Reply::WorktreePreparation(
                        WorktreePreparation::Preparing | WorktreePreparation::Busy,
                    ))) => {}
                    Some(Ok(Reply::WorktreePreparation(WorktreePreparation::Ready))) => {
                        this.update(cx, |d, cx| {
                            d.preparation_ready = true;
                            cx.notify();
                        })
                        .ok();
                    }
                    _ => {
                        this.update(cx, Dialog::cancel_preparation).ok();
                        return;
                    }
                }
                cx.background_executor()
                    .timer(Duration::from_millis(250))
                    .await;
            }
        }));
    }

    fn request(&self, setup: Setup, cx: &Context<Self>) -> Result<NewWorktree, &'static str> {
        let name: String = self
            .name
            .read(cx)
            .value()
            .trim()
            .chars()
            .filter(|c| !c.is_control())
            .collect();
        if name.is_empty() {
            return Err(text::EMPTY_NAME.get());
        }
        let base = self.base.read(cx).value().trim().to_owned();
        Ok(NewWorktree {
            repo: self.repo.as_os_str().as_bytes().to_vec(),
            name,
            base: (!base.is_empty()).then_some(base),
            setup,
        })
    }

    /// 입력값으로 요청을 보낸다. setup 승인·건너뛰기는 같은 입력으로 다시 보낸다(입력은 그동안 숨겨져 있다).
    fn submit(&mut self, setup: Setup, cx: &mut Context<Self>) {
        let req = match self.request(setup, cx) {
            Ok(req) => req,
            Err(e) => {
                self.phase = Phase::Edit(Some(e.to_owned()));
                cx.notify();
                return;
            }
        };
        self.phase = Phase::Creating;
        cx.notify();
        let source = self.source.clone();
        self.task = Some(cx.spawn(async move |this, cx| {
            let bg = cx.background_executor().clone();
            let reply = bg.spawn(async move { source.new_worktree(req) }).await;
            this.update(cx, |d, cx| d.finish(reply, cx)).ok();
        }));
    }

    fn finish(&mut self, reply: std::io::Result<Reply>, cx: &mut Context<Self>) {
        self.phase = match reply {
            Ok(Reply::WorktreeCreated(w)) => {
                self.cancel_preparation(cx);
                cx.emit(Event::Created(w.clone()));
                Phase::Done(w)
            }
            Ok(Reply::SetupUntrusted { script, hash }) => Phase::Untrusted { script, hash },
            Ok(Reply::WorktreeFailed(e)) => Phase::Edit(Some(e)),
            Ok(Reply::Failed(f)) => Phase::Edit(Some(f.to_string())),
            Ok(_) => Phase::Edit(Some(text::UNEXPECTED.get().to_owned())),
            Err(e) if e.kind() == std::io::ErrorKind::Unsupported => {
                Phase::Edit(Some(text::UNSUPPORTED.get().to_owned()))
            }
            Err(e) => Phase::Edit(Some(e.to_string())),
        };
        cx.notify();
    }
}

impl Render for Dialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.language != crate::i18n::language() {
            self.language = crate::i18n::language();
            self.name.update(cx, |input, cx| {
                input.set_placeholder(text::NAME.get(), window, cx);
            });
            self.base.update(cx, |input, cx| {
                input.set_placeholder(text::BASE.get(), window, cx);
            });
        }
        let p = crate::ui::palette(cx);
        let (warning, muted) = (p.warning, p.muted_foreground);
        let close = Button::new("worktree-close")
            .ghost()
            .label(text::CLOSE.get())
            .on_click(cx.listener(|_, _, _, cx| cx.emit(Event::Close)));
        let footer = || div().flex().flex_wrap().justify_end().gap_2();
        let state_dir = crate::settings::state_dir(cx);
        let repo_hooks =
            krill_proto::orchestration::inspect_repo_hooks(&self.repo, state_dir.as_deref());
        let body = div().flex().flex_col().gap_4();
        let body = match &self.phase {
            Phase::Edit(error) => body
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(crate::ui::field_label(text::NAME_LABEL.get(), None, p))
                        .child(Input::new(&self.name).h(px(32.))),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(crate::ui::field_label(
                            text::BASE_LABEL.get(),
                            Some(text::OPTIONAL.get()),
                            p,
                        ))
                        .child(Input::new(&self.base).h(px(32.)))
                        .when(self.preparation.is_some(), |d| {
                            d.child(
                                div()
                                    .flex()
                                    .gap_1()
                                    .items_center()
                                    .text_size(crate::ui::TEXT_2XS)
                                    .text_color(muted)
                                    .child(if self.preparation_ready {
                                        text::PREPARED.get()
                                    } else {
                                        text::PREPARING.get()
                                    })
                                    .child(
                                        Button::new("cancel-preparation")
                                            .ghost()
                                            .xsmall()
                                            .label(text::CANCEL_PREPARATION.get())
                                            .on_click(
                                                cx.listener(|d, _, _, cx| d.cancel_preparation(cx)),
                                            ),
                                    ),
                            )
                        }),
                )
                .when(!repo_hooks.effective_symlinks.is_empty(), |d| {
                    let paths = repo_hooks.effective_symlinks.join(", ");
                    d.child(
                        div()
                            .id("worktree-symlinks-preview")
                            .debug_selector(|| "worktree-symlinks-preview".into())
                            .font_family(crate::ui::FONT_MONO)
                            .text_size(crate::ui::TEXT_2XS)
                            .text_color(muted)
                            .child(text::SYMLINKS_PREVIEW.format(&[("paths", &paths)])),
                    )
                })
                .when_some(repo_hooks.effective_setup.clone(), |d, script| {
                    d.child(
                        div()
                            .id("worktree-setup-preview")
                            .debug_selector(|| "worktree-setup-preview".into())
                            .font_family(crate::ui::FONT_MONO)
                            .text_size(crate::ui::TEXT_2XS)
                            .text_color(muted)
                            .child(text::SETUP_PREVIEW.format(&[("script", &visible(&script))])),
                    )
                })
                .when_some(repo_hooks.effective_archive.clone(), |d, script| {
                    d.child(
                        div()
                            .id("worktree-archive-preview")
                            .debug_selector(|| "worktree-archive-preview".into())
                            .font_family(crate::ui::FONT_MONO)
                            .text_size(crate::ui::TEXT_2XS)
                            .text_color(muted)
                            .child(text::ARCHIVE_PREVIEW.format(&[("script", &visible(&script))])),
                    )
                })
                .children(
                    error
                        .clone()
                        .map(|e| crate::ui::error_banner(p, crate::i18n::cached_notice(&e))),
                )
                .child(
                    footer().child(close).child(
                        Button::new("worktree-create")
                            .primary()
                            .label(text::CREATE.get())
                            .on_click(cx.listener(|d, _, _, cx| d.submit(Setup::Ask, cx))),
                    ),
                ),
            Phase::Creating => body
                .child(
                    div()
                        .text_size(crate::ui::TEXT_XS)
                        .text_color(muted)
                        .child(text::CREATING.get()),
                )
                .child(footer().child(close)),
            Phase::Untrusted { script, hash } => {
                let approve = hash.clone();
                body.child(
                    div()
                        .text_size(crate::ui::TEXT_XS)
                        .text_color(warning)
                        .child(text::UNTRUSTED.get()),
                )
                .child(
                    div()
                        .id("setup-script")
                        .max_h(px(240.))
                        .overflow_y_scroll()
                        .p_2()
                        .rounded(crate::ui::RADIUS_XS)
                        .bg(p.muted)
                        .font_family(crate::ui::FONT_MONO)
                        .text_size(crate::ui::TEXT_XS)
                        .child(visible(script)),
                )
                .child(
                    div()
                        .font_family(crate::ui::FONT_MONO)
                        .text_size(crate::ui::TEXT_2XS)
                        .text_color(muted)
                        .child(crate::i18n::catalog::general::HASH.format(&[("hash", &hex(hash))])),
                )
                .child(
                    footer()
                        .child(close)
                        .child(
                            Button::new("setup-skip")
                                .ghost()
                                .label(text::SKIP.get())
                                .on_click(cx.listener(|d, _, _, cx| d.submit(Setup::Skip, cx))),
                        )
                        .child(
                            Button::new("setup-approve")
                                .danger()
                                .label(text::APPROVE.get())
                                .on_click(cx.listener(move |d, _, _, cx| {
                                    d.submit(Setup::Approve(approve.clone()), cx);
                                })),
                        ),
                )
            }
            Phase::Done(w) => body
                .child(
                    div()
                        .text_size(crate::ui::TEXT_XS)
                        .text_color(p.success)
                        .child(text::CREATED.get()),
                )
                .child(
                    div()
                        .font_family(crate::ui::FONT_MONO)
                        .text_size(crate::ui::TEXT_XS)
                        .text_color(muted)
                        .child(
                            PathBuf::from(std::ffi::OsStr::from_bytes(&w.path))
                                .display()
                                .to_string(),
                        ),
                )
                .child(
                    div()
                        .font_family(crate::ui::FONT_MONO)
                        .text_size(crate::ui::TEXT_XS)
                        .child(format!("{} ← {}", w.branch, w.base)),
                )
                .children(w.setup.map(|id| format!("{} {}", text::SETUP.get(), id.0)))
                .children(w.warnings.iter().map(|e| {
                    div()
                        .text_size(crate::ui::TEXT_XS)
                        .text_color(warning)
                        .child(e.clone())
                }))
                .child(footer().child(close)),
        };
        let card = div()
            .id("worktree-dialog")
            .occlude()
            .w(crate::ui::DIALOG_WIDTH)
            .max_w(window.viewport_size().width - crate::ui::SPACE_4 * 2.)
            .p_6()
            .rounded(crate::ui::RADIUS_LG)
            .bg(p.popover)
            .text_color(p.popover_foreground)
            .border_1()
            .border_color(p.border)
            .shadow_lg()
            .flex()
            .flex_col()
            .gap_4()
            .capture_key_down(cx.listener(|_, e: &gpui::KeyDownEvent, _, cx| {
                if e.keystroke.key == "escape" {
                    cx.stop_propagation();
                    cx.emit(Event::Close);
                }
            }))
            .on_mouse_down_out(cx.listener(|_, _, _, cx| cx.emit(Event::Close)))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .text_size(crate::ui::TEXT_LG)
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child(text::TITLE.get()),
                    )
                    .child(
                        div().text_size(crate::ui::TEXT_XS).text_color(muted).child(
                            self.repo
                                .file_name()
                                .unwrap_or_default()
                                .to_string_lossy()
                                .into_owned(),
                        ),
                    )
                    .when_some(self.parent.as_ref(), |d, parent| {
                        let parent_name = parent
                            .file_name()
                            .unwrap_or(parent.as_os_str())
                            .to_string_lossy()
                            .into_owned();
                        d.child(
                            div()
                                .id("worktree-parent")
                                .debug_selector(|| "worktree-parent".into())
                                .text_size(crate::ui::TEXT_2XS)
                                .text_color(muted)
                                .child(format!("{}: {parent_name}", text::PARENT_LABEL.get())),
                        )
                    }),
            )
            .child(body);
        crate::ui::modal_backdrop(window.viewport_size(), card)
    }
}

#[cfg(test)]
#[path = "worktree/tests.rs"]
mod tests;
