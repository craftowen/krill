//! MK-055: 폴더 선택·에이전트 실행. 외부 앱 열기(Open in)는 사이드바 행 메뉴(OWE-576).
//! OWE-723: 프로젝트 추가 모달의 복제·새 프로젝트는 새 탭의 셸에서 git을 돌린다(진행·인증·오류가 터미널에 보이고,
//! 끝나면 그 폴더의 로그인 셸이 남아 사이드바가 실제 cwd로 프로젝트를 찾는다).

use krill_proto::os::ffi::{OsStrExt as _, OsStringExt as _};
use std::fmt::Write as _;
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

    pub(super) fn new_folder_tab_on(
        &mut self,
        host: krill_app::hosts::HostId,
        folder: &std::path::Path,
        program: Program,
        cx: &mut Context<Self>,
    ) {
        let is_remote = host.is_remote();
        let project_pending = project_destination(&program).is_some();
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
        if !is_remote && !project_pending {
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
                        this.new_folder_tab_on(
                            krill_app::hosts::HostId::Local,
                            &folder,
                            Program::Login,
                            cx,
                        );
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
                        let program = clone_program(
                            target,
                            url.as_bytes(),
                            dir.as_bytes(),
                            parent.as_os_str().as_bytes(),
                        );
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

    pub(super) fn new_local_project_tab(
        &mut self,
        parent: &std::path::Path,
        program: Program,
        cx: &mut Context<Self>,
    ) {
        self.new_folder_tab_on(krill_app::hosts::HostId::Local, parent, program, cx);
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
                let program = clone_program(
                    TargetOs::local(),
                    url.as_bytes(),
                    dir.as_bytes(),
                    parent.as_os_str().as_bytes(),
                );
                this.update(cx, |this, cx| {
                    this.new_local_project_tab(&parent, program, cx);
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
                    this.new_local_project_tab(parent, program, cx);
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

const PROJECT_MARKER: &str = "# krill-project-v1:";

/// Windows 기본 셸에서 git을 실행하고 성공한 프로젝트 폴더에 남는다. 사용자 바이트는 Base64 데이터로만 넣어
/// 따옴표·공백·PowerShell 연산자로 해석되지 않게 한다. 첫 줄의 hex 데이터는 복원 후에도 프로젝트 작업을 식별한다.
fn windows_project(args: &[&[u8]], destination: &[u8]) -> Program {
    let decode = |value: &[u8]| {
        let encoded = super::security::base64_encode(value);
        format!(
            "$utf8.GetString([Convert]::FromBase64String('{}'))",
            String::from_utf8_lossy(&encoded)
        )
    };
    let data = args
        .iter()
        .map(|arg| decode(arg))
        .collect::<Vec<_>>()
        .join(", ");
    let marker = project_marker(destination);
    let destination = decode(destination);
    let script = format!(
        "{marker}\n& {{\n\
         $ErrorActionPreference = 'Stop'\n\
         $utf8 = [System.Text.UTF8Encoding]::new($false, $true)\n\
         [Console]::OutputEncoding = $utf8\n\
         $gitArgs = @({data})\n\
         & git @gitArgs\n\
         if ($LASTEXITCODE -eq 0) {{\n\
           Set-Location -LiteralPath ({destination})\n\
           [Console]::Write([string][char]27 + ']9;9;' + (Get-Location).ProviderPath + [char]7)\n\
         }}\n}}"
    );
    Program::Custom {
        program: b"powershell.exe".to_vec(),
        args: vec![
            b"-NoLogo".to_vec(),
            b"-NoExit".to_vec(),
            b"-Command".to_vec(),
            script.into_bytes(),
        ],
    }
}

/// 저장소 복제. 성공하면 실제 프로젝트 폴더의 셸과 cwd 보고를 유지한다.
fn clone_program(target: TargetOs, url: &[u8], dir: &[u8], parent: &[u8]) -> Program {
    match target {
        TargetOs::Unix => unix_project(
            r#"cd -- "$3" && git clone -- "$1" "$2" && cd -- "$2""#,
            &[url, dir, parent],
            dir,
        ),
        TargetOs::Windows => windows_project(&[b"clone", b"--", url, dir], dir),
    }
}

/// 새 프로젝트 폴더 생성 + `git init`(`git init <경로>`는 폴더를 만든다).
fn init_program(target: TargetOs, path: &[u8]) -> Program {
    match target {
        TargetOs::Unix => unix_project(
            r#"mkdir -- "$1" && git -C "$1" init && cd -- "$1""#,
            &[path],
            path,
        ),
        TargetOs::Windows => windows_project(&[b"init", b"--", path], path),
    }
}

fn project_marker(destination: &[u8]) -> String {
    let mut marker = String::from(PROJECT_MARKER);
    for byte in destination {
        write!(&mut marker, "{byte:02x}").ok();
    }
    marker
}

fn unix_project(command: &str, args: &[&[u8]], destination: &[u8]) -> Program {
    let script = format!(
        r#"{}
if {command}; then
  encoded=$(printf '%s' "${{PWD#/}}" | od -An -v -tx1 | tr -d ' \n' | sed 's/../%&/g')
  printf '\033]7;file://localhost/%s\007' "$encoded"
fi
exec "${{SHELL:-/bin/sh}}" -l"#,
        project_marker(destination)
    );
    shell_script(&script, args)
}

/// Recognize only the structured bootstrap argument layouts emitted above.
fn project_destination(program: &Program) -> Option<Vec<u8>> {
    let Program::Custom { program, args } = program else {
        return None;
    };
    let script = if program.eq_ignore_ascii_case(b"powershell.exe")
        && args.len() == 4
        && args.first()?.as_slice() == b"-NoLogo"
        && args.get(1)?.as_slice() == b"-NoExit"
        && args.get(2)?.as_slice() == b"-Command"
    {
        args.last()?
    } else if program == b"/bin/sh"
        && matches!(args.len(), 4..=6)
        && args.first()?.as_slice() == b"-c"
        && args.get(2)?.as_slice() == b"krill"
    {
        args.get(1)?
    } else {
        return None;
    };
    let hex = std::str::from_utf8(script)
        .ok()?
        .lines()
        .next()?
        .strip_prefix(PROJECT_MARKER)?;
    let (pairs, remainder) = hex.as_bytes().as_chunks::<2>();
    if pairs.is_empty() || !remainder.is_empty() {
        return None;
    }
    pairs
        .iter()
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok())
        .collect()
}

/// Git 작업이 실제 프로젝트로 이동한 뒤에만 영속 실행 폴더를 바꾼다. 실행 전에는 존재하는 상위 cwd가 필요하다.
/// 일반 셸의 cd는 워크스페이스를 바꾸지 않는 기존 규칙을 유지한다.
pub(super) fn completed_project(launch: &Create, live_cwd: &std::path::Path) -> Option<Create> {
    let destination = project_destination(&launch.program)?;
    let parent = PathBuf::from(std::ffi::OsString::from_vec(launch.cwd.clone()));
    let folder = PathBuf::from(std::ffi::OsString::from_vec(destination.clone()));
    let folder = if is_windows_path(&destination) {
        folder
    } else {
        parent.join(folder)
    };
    if !krill_proto::path::equivalent(&folder, live_cwd) {
        return None;
    }
    let mut completed = launch.clone();
    completed.cwd = live_cwd.as_os_str().as_bytes().to_vec();
    completed.program = Program::Login;
    Some(completed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unix_targets_keep_the_login_shell_script_with_positional_args() {
        let url = b"https://example.com/a;rm -rf $HOME.git";
        let Program::Custom { program, args } = clone_program(TargetOs::Unix, url, b"a", b"/work")
        else {
            panic!("custom program");
        };
        assert_eq!(program, b"/bin/sh");
        assert_eq!(args[0], b"-c");
        assert_eq!(args[2], b"krill");
        assert_eq!(args[3], url, "user input stays a positional argument");
        assert_eq!(args[4], b"a");
        assert_eq!(args[5], b"/work");
        let Program::Custom { program, args } = init_program(TargetOs::Unix, b"/tmp/p q") else {
            panic!("custom program");
        };
        assert_eq!(program, b"/bin/sh");
        assert_eq!(args.last().map(Vec::as_slice), Some(&b"/tmp/p q"[..]));
    }

    /// 사용자 URL·경로는 PowerShell 코드가 아니라 데이터이며 작업 뒤에는 셸이 남는다.
    #[test]
    fn windows_project_shell_preserves_data_and_reports_the_project_directory() {
        let url = b"https://example.com/a;echo $(whoami).git";
        let Program::Custom { program, args } =
            clone_program(TargetOs::Windows, url, b"a", b"C:\\work")
        else {
            panic!("custom program");
        };
        assert_eq!(program, b"powershell.exe");
        assert_eq!(args[1], b"-NoExit");
        let script = std::str::from_utf8(args.last().unwrap()).unwrap();
        assert!(!script.contains(std::str::from_utf8(url).unwrap()));
        assert!(
            script
                .contains(&String::from_utf8(super::super::security::base64_encode(url)).unwrap())
        );
        assert!(script.contains("& git @gitArgs"));
        assert!(script.contains("Set-Location -LiteralPath"));
        assert!(script.contains("']9;9;'"));
        assert!(script.contains("(Get-Location).ProviderPath"));
        assert!(script.contains("[Console]::OutputEncoding = $utf8"));
        let Program::Custom { program, args } =
            init_program(TargetOs::Windows, b"C:\\Users\\dev\\p q")
        else {
            panic!("custom program");
        };
        assert_eq!(program, b"powershell.exe");
        assert_eq!(args[1], b"-NoExit");
        let launch = Create {
            account: None,
            program: clone_program(TargetOs::Windows, url, b"p q", b"C:\\Users\\dev"),
            cwd: b"C:\\Users\\dev".to_vec(),
            cols: 80,
            rows: 24,
        };
        assert!(completed_project(&launch, std::path::Path::new(r"C:\Users\dev")).is_none());
        let completed =
            completed_project(&launch, std::path::Path::new("c:/Users/dev/p q")).unwrap();
        assert_eq!(completed.cwd, b"c:/Users/dev/p q");
        assert_eq!(completed.program, Program::Login);
        assert_eq!(
            TargetOs::local(),
            if cfg!(windows) {
                TargetOs::Windows
            } else {
                TargetOs::Unix
            }
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_project_commands_create_and_clone_with_literal_paths() {
        use std::process::{Command, Stdio};
        use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
        let root = std::env::temp_dir().join(format!(
            "krill-project-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let run = |program: Program| {
            let Program::Custom { program, args } = program else {
                panic!("custom program")
            };
            // 같은 초기화 코드를 유한하게 실행한다. 대화형 셸 유지는 위 테스트의 NoExit 인자로 고정한다.
            let args: Vec<_> = args
                .into_iter()
                .filter(|arg| arg != b"-NoExit")
                .map(std::ffi::OsString::from_vec)
                .collect();
            let mut child = Command::new(std::ffi::OsString::from_vec(program))
                .args(args)
                .current_dir(&root)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            let deadline = Instant::now() + Duration::from_secs(30);
            loop {
                if child.try_wait().unwrap().is_some() {
                    break;
                }
                if Instant::now() >= deadline {
                    child.kill().ok();
                    child.wait().ok();
                    panic!("Windows project command timed out");
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            let output = child.wait_with_output().unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            let text = String::from_utf8(output.stdout).unwrap();
            assert!(text.contains("\u{1b}]9;9;"));
            text
        };
        let original = root.join("original ' $(New-Item PWNED) & 한글");
        let created = run(init_program(
            TargetOs::Windows,
            original.as_os_str().as_bytes(),
        ));
        assert!(original.join(".git").is_dir());
        assert!(created.contains(original.file_name().unwrap().to_str().unwrap()));
        let clone = "clone ' $(New-Item PWNED) & 한글";
        let cloned = run(clone_program(
            TargetOs::Windows,
            original.as_os_str().as_bytes(),
            clone.as_bytes(),
            root.as_os_str().as_bytes(),
        ));
        assert!(root.join(clone).join(".git").is_dir());
        assert!(cloned.contains(clone));
        assert!(!root.join("PWNED").exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn project_completion_supports_unix_and_unc_and_rejects_other_cwds() {
        for (target, parent, destination, observed) in [
            (TargetOs::Unix, "/work", "/work/new p", "/work/new p"),
            (TargetOs::Unix, "/work", "new p", "/work/new p"),
            (
                TargetOs::Windows,
                r"\\nas\share",
                r"\\nas\share\new p",
                r"\\nas\share\new p",
            ),
        ] {
            let launch = Create {
                account: None,
                program: init_program(target, destination.as_bytes()),
                cwd: parent.as_bytes().to_vec(),
                cols: 80,
                rows: 24,
            };
            assert!(completed_project(&launch, std::path::Path::new(parent)).is_none());
            let done = completed_project(&launch, std::path::Path::new(observed)).unwrap();
            assert_eq!(done.program, Program::Login);
            assert_eq!(done.cwd, observed.as_bytes());
        }
    }

    #[cfg(unix)]
    #[test]
    fn unix_project_bootstraps_report_only_success_and_keep_literal_paths() {
        use std::os::unix::fs::PermissionsExt as _;
        use std::process::Command;
        let root = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("krill-project-posix-{}", std::process::id()));
        std::fs::remove_dir_all(&root).ok();
        std::fs::create_dir_all(&root).unwrap();
        let run = |program: Program| {
            let Program::Custom { program, args } = program else {
                panic!("custom bootstrap");
            };
            Command::new(std::ffi::OsString::from_vec(program))
                .args(args.into_iter().map(std::ffi::OsString::from_vec))
                .current_dir(&root)
                .env("SHELL", "/usr/bin/true")
                .env("PWD", "/mismatched-holder-startup-pwd")
                .output()
                .unwrap()
        };
        let original = root.join("original ' % # ; $(touch PWNED) & 한글");
        let output = run(init_program(
            TargetOs::Unix,
            original.as_os_str().as_bytes(),
        ));
        assert!(output.status.success());
        assert!(original.join(".git").is_dir());
        let cwd_frame = |path: &std::path::Path| {
            let mut encoded = String::new();
            for byte in path.as_os_str().as_bytes().iter().skip(1) {
                write!(&mut encoded, "%{byte:02x}").unwrap();
            }
            format!("\u{1b}]7;file://localhost/{encoded}\u{7}")
        };
        assert!(String::from_utf8_lossy(&output.stdout).contains(&cwd_frame(&original)));
        let clone = "clone ' % # ; $(touch PWNED) & 한글";
        let output = run(clone_program(
            TargetOs::Unix,
            original.as_os_str().as_bytes(),
            clone.as_bytes(),
            root.as_os_str().as_bytes(),
        ));
        assert!(output.status.success());
        assert!(root.join(clone).join(".git").is_dir());
        assert!(String::from_utf8_lossy(&output.stdout).contains(&cwd_frame(&root.join(clone))));
        let logical_parent = root.join("logical-parent");
        std::os::unix::fs::symlink(&root, &logical_parent).unwrap();
        let program = clone_program(
            TargetOs::Unix,
            original.as_os_str().as_bytes(),
            b"symlink-clone",
            logical_parent.as_os_str().as_bytes(),
        );
        let launch = Create {
            account: None,
            program: program.clone(),
            cwd: logical_parent.as_os_str().as_bytes().to_vec(),
            cols: 80,
            rows: 24,
        };
        let output = run(program);
        let logical_destination = logical_parent.join("symlink-clone");
        assert!(root.join("symlink-clone/.git").is_dir());
        assert!(String::from_utf8_lossy(&output.stdout).contains(&cwd_frame(&logical_destination)));
        assert_eq!(
            completed_project(&launch, &logical_destination)
                .unwrap()
                .program,
            Program::Login
        );
        let output = run(clone_program(
            TargetOs::Unix,
            root.join("missing-repo").as_os_str().as_bytes(),
            b"never-created",
            root.as_os_str().as_bytes(),
        ));
        assert!(!root.join("never-created").exists());
        assert!(!String::from_utf8_lossy(&output.stdout).contains("\u{1b}]7;"));
        assert!(!root.join("PWNED").exists());
        // A git-init failure must leave the fallback login shell in the parent,
        // otherwise a normal prompt's cwd report could falsely register it.
        let fake_bin = root.join("fake-bin");
        std::fs::create_dir_all(&fake_bin).unwrap();
        std::fs::write(fake_bin.join("git"), "#!/bin/sh\nexit 1\n").unwrap();
        let login = root.join("login-shell");
        std::fs::write(&login, "#!/bin/sh\nprintf '%s\\n' \"$PWD\"\n").unwrap();
        for path in [fake_bin.join("git"), login.clone()] {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let failed = root.join("failed-init");
        let Program::Custom { program, args } =
            init_program(TargetOs::Unix, failed.as_os_str().as_bytes())
        else {
            panic!("bootstrap");
        };
        let output = Command::new(std::ffi::OsString::from_vec(program))
            .args(args.into_iter().map(std::ffi::OsString::from_vec))
            .current_dir(&root)
            .env("SHELL", &login)
            .env("PATH", format!("{}:/usr/bin:/bin", fake_bin.display()))
            .output()
            .unwrap();
        assert!(output.status.success());
        assert_eq!(
            String::from_utf8(output.stdout).unwrap().trim(),
            root.to_str().unwrap()
        );
        assert!(failed.is_dir());
        assert!(!failed.join(".git").exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[gpui::test]
    fn local_project_waits_for_successful_cwd_before_registering_and_ignores_remote_launch_host(
        cx: &mut gpui::TestAppContext,
    ) {
        use krill_app::hosts::HostId;
        use krill_app::sidebar;
        use krill_proto::SessionId;
        use std::sync::Arc;
        struct Memory(PathBuf);
        impl sidebar::Source for Memory {
            fn fetch(&self) -> std::io::Result<sidebar::Snapshot> {
                Ok(sidebar::Snapshot {
                    sessions: vec![sidebar::Session {
                        id: SessionId(42),
                        status: krill_proto::control::AgentStatus::UNKNOWN,
                        worktree: None,
                        cwd: Some(self.0.clone()),
                        account: None,
                    }],
                    trees: std::collections::BTreeMap::new(),
                })
            }
        }
        let state = crate::shell::tests::state_path("project-register-success");
        let state_dir = state.parent().unwrap().to_path_buf();
        let parent = state_dir.join("parent");
        std::fs::create_dir_all(&parent).unwrap();
        let destination = parent.join("actual-project");
        let projects = state_dir.join("projects.toml");
        let (view, cx) = crate::shell::tests::open(cx, state);
        let pane = view.update(cx, |shell, cx| {
            shell.launch_host = HostId::Remote("previous-dialog-choice".into());
            shell.new_local_project_tab(
                &parent,
                init_program(TargetOs::Unix, destination.as_os_str().as_bytes()),
                cx,
            );
            let pane = shell.layout.focused().unwrap();
            assert_eq!(shell.layout.host(pane), &HostId::Local);
            shell.layout.set_session(pane, SessionId(42));
            shell.sidebar.update(cx, |sidebar, cx| {
                sidebar.connect_source(Arc::new(Memory(parent.clone())), cx);
            });
            pane
        });
        cx.run_until_parked();
        assert!(
            !projects.exists(),
            "neither the parent nor a failed destination is registered"
        );
        view.read_with(cx, |shell, _| {
            assert!(matches!(
                shell.layout.launch(pane).unwrap().program,
                Program::Custom { .. }
            ));
        });
        std::fs::create_dir_all(&destination).unwrap();
        view.update(cx, |shell, cx| {
            shell.sidebar.update(cx, |sidebar, cx| {
                sidebar.connect_source(Arc::new(Memory(destination.clone())), cx);
            });
        });
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while !projects.exists() && std::time::Instant::now() < deadline {
            cx.run_until_parked();
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        cx.run_until_parked();
        let content = std::fs::read_to_string(&projects).unwrap();
        let value: toml::Value = toml::from_str(&content).unwrap();
        let entries = value
            .get("projects")
            .and_then(toml::Value::as_array)
            .unwrap();
        assert_eq!(
            entries.len(),
            1,
            "only completed project is registered: {content}"
        );
        assert_eq!(
            entries[0].get("path").and_then(toml::Value::as_str),
            destination.canonicalize().unwrap().to_str()
        );
        assert!(content.contains("actual-project"));
        view.read_with(cx, |shell, _| {
            assert_eq!(shell.layout.launch(pane).unwrap().program, Program::Login);
            assert_eq!(
                shell.layout.launch(pane).unwrap().cwd,
                destination.as_os_str().as_bytes()
            );
        });
        let later = destination.join("subdir");
        std::fs::create_dir_all(&later).unwrap();
        view.update(cx, |shell, cx| {
            shell.sidebar.update(cx, |sidebar, cx| {
                sidebar.connect_source(Arc::new(Memory(later.clone())), cx);
            });
        });
        cx.run_until_parked();
        view.read_with(cx, |shell, _| {
            assert_eq!(
                shell.layout.launch(pane).unwrap().cwd,
                destination.as_os_str().as_bytes()
            );
            assert_eq!(
                shell.layout.respawn_cwd(pane),
                Some(later.as_os_str().as_bytes())
            );
        });
        assert_eq!(
            std::fs::read_to_string(&projects).unwrap(),
            content,
            "ordinary cd never registers a new project"
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
