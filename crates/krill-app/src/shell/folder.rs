//! MK-055: 폴더 선택·에이전트 실행. 외부 앱 열기(Open in)는 사이드바 행 메뉴(OWE-576).
//! OWE-723: 프로젝트 추가 모달의 복제·새 프로젝트는 새 탭의 셸에서 git을 돌린다(진행·인증·오류가 터미널에 보이고,
//! 끝나면 그 폴더의 로그인 셸이 남아 사이드바가 실제 cwd로 프로젝트를 찾는다).

use krill_proto::os::ffi::{OsStrExt as _, OsStringExt as _};
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
        let is_remote = self.layout.host(pane).is_remote();
        let mut create = self.layout.launch(pane).cloned().unwrap_or_else(|| {
            let cwd = if is_remote {
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
        });
        if !is_remote && let Ok(text) = std::str::from_utf8(&create.cwd) {
            let cleaned = krill_proto::orchestration::clean_workspace_path_str(text);
            if cleaned != text {
                create.cwd = cleaned.into_bytes();
            }
        }
        create
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
        let is_remote = host.is_remote();
        let cleaned = (!is_remote)
            .then(|| folder.to_str())
            .flatten()
            .and_then(|s| {
                krill_proto::orchestration::strip_workspace_instance(s).filter(|_| !folder.exists())
            })
            .map(PathBuf::from);
        let folder = cleaned.as_deref().unwrap_or(folder);
        if matches!(program, Program::Agent { .. }) {
            krill_app::star_nag::record_agent_start(cx);
        }
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
        // OWE-852: 로컬 폴더를 열면 projects.toml에 영속 등록해 탭을 닫아도 사이드바에 남긴다.
        // 원격 호스트 경로는 로컬 projects.toml에 넣지 않는다. 실패해도 탭 열기는 막지 않는다.
        if !is_remote {
            self.sidebar.update(cx, |sidebar, cx| {
                sidebar.register_project(folder.to_owned(), cx);
            });
        }
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
        self.close_dashboard(None, cx);
        let hosts = self.execution_locations(cx);
        let initial = self.launch_host.clone();
        let dialog = cx.new(|cx| add_project::Dialog::new_with_hosts(window, cx, hosts, initial));
        let events = cx.subscribe_in(
            &dialog,
            window,
            |this, _, e: &add_project::Event, window, cx| {
                this.add_project = None;
                window.focus(&this.focus, cx);
                cx.notify();
                match e {
                    add_project::Event::Browse(host) => {
                        if host.is_local() {
                            Self::choose_folder(cx);
                        }
                    }
                    add_project::Event::OpenPath(host, path) => {
                        this.new_folder_tab_on(host.clone(), path, Program::Login, cx);
                    }
                    add_project::Event::Clone { host, url } => {
                        if host.is_local() {
                            Self::clone_project(url.clone(), cx);
                        }
                    }
                    add_project::Event::CloneAt { host, url, parent } => {
                        let Some(dir) = add_project::clone_dir(url) else {
                            return;
                        };
                        let target = this.target_os(host, parent.as_os_str().as_bytes());
                        let program = clone_program(target, url.as_bytes(), dir.as_bytes());
                        this.new_folder_tab_on(host.clone(), parent, program, cx);
                    }
                    add_project::Event::Create(host) => {
                        if host.is_local() {
                            Self::create_project(cx);
                        }
                    }
                    add_project::Event::CreateAt(host, path) => {
                        let path_bytes = path.as_os_str().as_bytes();
                        let target = this.target_os(host, path_bytes);
                        let program = init_program(target, path_bytes);
                        // 새 칸의 cwd는 상위 폴더. Windows 경로는 이 앱(POSIX)의 `Path::parent`가 `\`를 모르므로
                        // 바이트로 자른다.
                        let parent = match target {
                            TargetOs::Windows => PathBuf::from(std::ffi::OsString::from_vec(
                                windows_parent(path_bytes).to_vec(),
                            )),
                            TargetOs::Unix => path
                                .parent()
                                .map_or_else(|| PathBuf::from("/"), std::path::Path::to_path_buf),
                        };
                        this.new_folder_tab_on(host.clone(), &parent, program, cx);
                    }
                    add_project::Event::Close => {}
                }
            },
        );
        self.add_project = Some((dialog, events));
        cx.notify();
    }

    /// 새 세션이 실행될 OS. 로컬은 이 앱의 OS. 원격은 (1) SSH 중계 터널이 probe로 보고한 OS 문자열(중계 자체는
    /// 전송 방식일 뿐이라 OS 증거로 쓰지 않는다), (2) 터널 항목이 없는 P2P 원격을 위해 사용자가 고른 경로의
    /// Windows 문법(`C:\`·`C:/`·UNC). 소켓 포워딩 원격은 POSIX다.
    fn target_os(&self, host: &krill_app::hosts::HostId, path: &[u8]) -> TargetOs {
        let windows = match host {
            krill_app::hosts::HostId::Local => cfg!(windows),
            krill_app::hosts::HostId::Remote(id) => {
                self.host_manager
                    .remote_os(id)
                    .is_some_and(|os| os.eq_ignore_ascii_case("windows"))
                    || is_windows_path(path)
            }
        };
        if windows {
            TargetOs::Windows
        } else {
            TargetOs::Unix
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
                let program = clone_program(TargetOs::local(), url.as_bytes(), dir.as_bytes());
                this.update(cx, |this, cx| {
                    this.new_folder_tab(&parent, program, cx);
                    // OWE-852: 복제될 실제 저장소 루트(parent/dir)를 영속 등록한다.
                    this.sidebar.update(cx, |sidebar, cx| {
                        sidebar.register_project(parent.join(&dir), cx);
                    });
                })
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
                let program = init_program(TargetOs::local(), path.as_os_str().as_bytes());
                this.update(cx, |this, cx| {
                    this.new_folder_tab(parent, program, cx);
                    // OWE-852: 새로 만들 프로젝트 루트 자체를 영속 등록한다.
                    this.sidebar
                        .update(cx, |sidebar, cx| sidebar.register_project(path.clone(), cx));
                })
                .ok();
            }
        })
        .detach();
    }
}

/// Add Project 명령이 실행될 OS(APP-RUNTIME-006).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TargetOs {
    Unix,
    Windows,
}

impl TargetOs {
    fn local() -> Self {
        if cfg!(windows) {
            Self::Windows
        } else {
            Self::Unix
        }
    }
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

/// Windows 경로 문법: 드라이브 루트(`C:\`·`C:/`) 또는 UNC(`\\server\share`). POSIX 경로는 `/`로 시작한다.
fn is_windows_path(path: &[u8]) -> bool {
    matches!(path, [drive, b':', b'\\' | b'/', ..] if drive.is_ascii_alphabetic())
        || path.starts_with(b"\\\\")
}

/// Windows 경로의 상위 폴더(`\`·`/` 모두 구분자). 드라이브 루트는 구분자를 남기고(`C:\`), 구분자가 없으면 그대로.
fn windows_parent(path: &[u8]) -> &[u8] {
    match path.iter().rposition(|b| matches!(b, b'\\' | b'/')) {
        Some(0) | None => path,
        Some(i) => {
            let (parent, _) = path.split_at(i);
            if parent.ends_with(b":") {
                path.split_at(i.saturating_add(1)).0
            } else {
                parent
            }
        }
    }
}

/// 셸 없이 `git`을 직접 실행한다(인자는 그대로 argv, 셸 해석 없음). Windows에는 `/bin/sh`가 없어 셸 스크립트를
/// 띄울 수 없고, krilld가 PATH에서 `git.exe`를 찾는다.
fn git_direct(args: &[&[u8]]) -> Program {
    Program::Custom {
        program: b"git".to_vec(),
        args: args.iter().map(|a| a.to_vec()).collect(),
    }
}

/// 저장소 복제. Unix는 끝나면 그 폴더의 로그인 셸이 남고, Windows는 git이 끝나면 칸의 프로세스가 끝난다
/// (사이드바에는 등록되므로 거기서 새 탭을 연다).
// ponytail: Windows에 "끝나면 셸 유지"는 없다(Program에 env가 없어 인용 없는 PowerShell 래퍼를 못 넘긴다).
// 필요해지면 상태 디렉터리에 `-File` 스크립트를 두고 인자로 넘긴다.
fn clone_program(target: TargetOs, url: &[u8], dir: &[u8]) -> Program {
    match target {
        TargetOs::Unix => shell_script(
            r#"git clone -- "$1" "$2" && cd -- "$2"; exec "${SHELL:-/bin/zsh}" -l"#,
            &[url, dir],
        ),
        TargetOs::Windows => git_direct(&[b"clone", b"--", url, dir]),
    }
}

/// 새 프로젝트 폴더 생성 + `git init`(`git init <경로>`는 폴더를 만든다).
fn init_program(target: TargetOs, path: &[u8]) -> Program {
    match target {
        TargetOs::Unix => shell_script(
            r#"mkdir -- "$1" && cd -- "$1" && git init; exec "${SHELL:-/bin/zsh}" -l"#,
            &[path],
        ),
        TargetOs::Windows => git_direct(&[b"init", b"--", path]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unix_targets_keep_the_login_shell_script_with_positional_args() {
        let url = b"https://example.com/a;rm -rf $HOME.git";
        let Program::Custom { program, args } = clone_program(TargetOs::Unix, url, b"a") else {
            panic!("custom program");
        };
        assert_eq!(program, b"/bin/sh");
        assert_eq!(args[0], b"-c");
        assert_eq!(args[2], b"krill");
        assert_eq!(args[3], url, "user input stays a positional argument");
        assert_eq!(args[4], b"a");
        let Program::Custom { program, args } = init_program(TargetOs::Unix, b"/tmp/p q") else {
            panic!("custom program");
        };
        assert_eq!(program, b"/bin/sh");
        assert_eq!(args.last().map(Vec::as_slice), Some(&b"/tmp/p q"[..]));
    }

    /// APP-RUNTIME-006: Windows에는 `/bin/sh`가 없다. 셸 없이 git을 직접 실행하고 URL·경로는 해석 없이 argv로 간다.
    #[test]
    fn windows_targets_run_git_directly_without_a_shell() {
        let url = b"https://example.com/a;echo $(whoami).git";
        let Program::Custom { program, args } = clone_program(TargetOs::Windows, url, b"a") else {
            panic!("custom program");
        };
        assert_eq!(program, b"git");
        assert_eq!(
            args,
            vec![
                b"clone".to_vec(),
                b"--".to_vec(),
                url.to_vec(),
                b"a".to_vec()
            ]
        );
        let Program::Custom { program, args } =
            init_program(TargetOs::Windows, b"C:\\Users\\dev\\p q")
        else {
            panic!("custom program");
        };
        assert_eq!(program, b"git");
        assert_eq!(
            args,
            vec![
                b"init".to_vec(),
                b"--".to_vec(),
                b"C:\\Users\\dev\\p q".to_vec()
            ]
        );
        assert_eq!(
            TargetOs::local(),
            if cfg!(windows) {
                TargetOs::Windows
            } else {
                TargetOs::Unix
            }
        );
    }

    /// 터널 항목이 없는 P2P 원격도 사용자가 고른 경로 문법으로 Windows 대상을 가려낸다.
    #[test]
    fn windows_path_syntax_marks_a_windows_target() {
        for path in [
            &b"C:\\Users\\dev\\src"[..],
            b"c:/Users/dev",
            b"D:\\",
            b"\\\\nas\\share\\projects",
        ] {
            assert!(is_windows_path(path), "{}", String::from_utf8_lossy(path));
        }
        for path in [
            &b"/home/dev/src"[..],
            b"/Users/dev",
            b"",
            b"relative\\dir",
            b"C:relative",
        ] {
            assert!(!is_windows_path(path), "{}", String::from_utf8_lossy(path));
        }
        assert_eq!(windows_parent(b"C:\\Users\\dev\\proj"), b"C:\\Users\\dev");
        assert_eq!(windows_parent(b"C:/Users/dev/proj"), b"C:/Users/dev");
        assert_eq!(windows_parent(b"C:\\proj"), b"C:\\");
        assert_eq!(windows_parent(b"\\\\nas\\share\\proj"), b"\\\\nas\\share");
        assert_eq!(windows_parent(b"proj"), b"proj");
    }
}
