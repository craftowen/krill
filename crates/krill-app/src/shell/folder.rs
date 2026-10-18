//! MK-055: 폴더 선택·에이전트 실행. 외부 앱 열기(Open in)는 사이드바 행 메뉴(OWE-576).
//! OWE-723: 프로젝트 추가 모달의 복제·새 프로젝트는 새 탭의 셸에서 git을 돌린다(진행·인증·오류가 터미널에 보이고,
//! 끝나면 그 폴더의 로그인 셸이 남아 사이드바가 실제 cwd로 프로젝트를 찾는다).

use std::os::unix::ffi::{OsStrExt as _, OsStringExt as _};
use std::path::PathBuf;

use gpui::{AppContext as _, Context, Window};
use krill_app::add_project;
use krill_app::layout::PaneId;
use krill_app::pane;
use krill_proto::control::{Create, Program};

use super::{Shell, text};

/// Finder에서 실행해도 새 칸은 사용자 홈에서 시작한다.
pub(super) fn default_folder() -> PathBuf {
    std::env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .map_or_else(|| PathBuf::from("/"), PathBuf::from)
}

impl Shell {
    /// 폴더를 지정하지 않은 기존 칸은 로컬이면 사용자 홈 디렉터리, 원격이면 원격 기본 디렉터리를 사용한다.
    pub(super) fn launch_for(&self, pane: PaneId) -> Create {
        self.layout.launch(pane).cloned().unwrap_or_else(|| {
            let cwd = if self.layout.host(pane).is_remote() {
                Vec::new()
            } else {
                default_folder().as_os_str().as_bytes().to_vec()
            };
            Create {
                account: None,
                program: Program::Login,
                cwd,
                cols: pane::SIZE.0,
                rows: pane::SIZE.1,
            }
        })
    }

    pub(super) fn folder(&self) -> PathBuf {
        self.layout.focused().map_or_else(default_folder, |p| {
            PathBuf::from(std::ffi::OsString::from_vec(self.launch_for(p).cwd))
        })
    }

    pub(super) fn new_folder_tab(
        &mut self,
        folder: &std::path::Path,
        program: Program,
        cx: &mut Context<Self>,
    ) {
        let host = self.launch_host.clone();
        self.new_folder_tab_on(host, folder, program, cx);
    }

    pub(super) fn new_folder_tab_on(
        &mut self,
        host: krill_app::hosts::HostId,
        folder: &std::path::Path,
        program: Program,
        cx: &mut Context<Self>,
    ) {
        self.change(true, cx, |l| {
            l.new_tab();
            if let Some(pane) = l.focused() {
                if host.is_remote() {
                    l.set_host(pane, host);
                }
                l.set_launch(
                    pane,
                    Create {
                        account: None,
                        program,
                        cwd: folder.as_os_str().as_bytes().to_vec(),
                        cols: pane::SIZE.0,
                        rows: pane::SIZE.1,
                    },
                );
            }
        });
    }

    pub(super) fn choose_folder(cx: &mut Context<Self>) {
        let chosen = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some(text::OPEN_FOLDER.get().into()),
        });
        cx.spawn(async move |this, cx| match chosen.await {
            Ok(Ok(Some(paths))) => {
                if let Some(folder) = paths.into_iter().next() {
                    this.update(cx, |this, cx| {
                        this.new_folder_tab(&folder, Program::Login, cx);
                    })
                    .ok();
                }
            }
            Ok(Err(e)) => {
                this.update(cx, |this, cx| {
                    this.notice = Some(e.to_string());
                    cx.notify();
                })
                .ok();
            }
            _ => {}
        })
        .detach();
    }

    pub(super) fn open_add_project(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let dialog = cx.new(|cx| add_project::Dialog::new(window, cx));
        let events = cx.subscribe_in(
            &dialog,
            window,
            |this, _, e: &add_project::Event, window, cx| {
                this.add_project = None;
                window.focus(&this.focus, cx);
                cx.notify();
                match e {
                    add_project::Event::Browse => Self::choose_folder(cx),
                    add_project::Event::Clone(url) => Self::clone_project(url.clone(), cx),
                    add_project::Event::Create => Self::create_project(cx),
                    add_project::Event::Close => {}
                }
            },
        );
        self.add_project = Some((dialog, events));
        cx.notify();
    }

    /// `sh -c script krill args…` — 사용자 입력은 위치 인자로만 넘겨 셸 해석을 피한다.
    fn shell_script(script: &str, args: &[&[u8]]) -> Program {
        let mut argv = vec![
            b"-c".to_vec(),
            script.as_bytes().to_vec(),
            b"krill".to_vec(),
        ];
        argv.extend(args.iter().map(|a| a.to_vec()));
        Program::Custom {
            program: b"/bin/sh".to_vec(),
            args: argv,
        }
    }

    fn clone_project(url: String, cx: &mut Context<Self>) {
        let Some(dir) = add_project::clone_dir(&url) else {
            return;
        };
        let chosen = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some(
                krill_app::i18n::catalog::add_project::CLONE_LOCATION
                    .get()
                    .into(),
            ),
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = chosen.await
                && let Some(parent) = paths.into_iter().next()
            {
                let program = Self::shell_script(
                    r#"git clone -- "$1" "$2" && cd -- "$2"; exec "${SHELL:-/bin/zsh}" -l"#,
                    &[url.as_bytes(), dir.as_bytes()],
                );
                this.update(cx, |this, cx| this.new_folder_tab(&parent, program, cx))
                    .ok();
            }
        })
        .detach();
    }

    fn create_project(cx: &mut Context<Self>) {
        let chosen = cx.prompt_for_new_path(&default_folder(), Some("my-project"));
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(path))) = chosen.await
                && let Some(parent) = path.parent()
            {
                let program = Self::shell_script(
                    r#"mkdir -- "$1" && cd -- "$1" && git init; exec "${SHELL:-/bin/zsh}" -l"#,
                    &[path.as_os_str().as_bytes()],
                );
                this.update(cx, |this, cx| this.new_folder_tab(parent, program, cx))
                    .ok();
            }
        })
        .detach();
    }
}
