//! 세션 셸의 argv와 자식 환경(MK-034). 결과는 holder `Spawn`의 `argv`·`env`에 그대로 들어간다.
//!
//! 목표는 세션 셸의 PATH(nvm·mise처럼 rc 파일이 만드는 것 포함)가 Terminal.app 창과 같은 것이다.
//! Terminal.app은 launchd가 GUI 앱에 주는 환경(PATH=`/usr/bin:/bin:/usr/sbin:/sbin`, 실측)을 받아
//! `login -pf <user>`로 로그인 셸을 띄우고, login(1)은 passwd로 HOME·USER·LOGNAME·SHELL을 채운다. 그래서
//!
//! - 기본 셸은 passwd의 로그인 셸을 `-l`로 띄운다. `zprofile`(`path_helper`)·`zshrc`·`zlogin`을 같은 순서로 읽는다.
//! - 환경은 krilld 것을 물려주지 않고 launchd 기본값만 남긴다. krilld가 개발 셸에서 떠도 그 셸의 PATH와
//!   nvm·mise 활성화 흔적(`NVM_BIN`, `__MISE_*` 등)·`MK_*` 내부 값이 새지 않아, rc 파일이 같은 입력에서
//!   같은 PATH를 만든다.
//!
//! Windows(OWE-833, windows-spike-OWE-830.md)는 로그인 셸이 없다.
//! - 기본 셸은 `PATH`의 [`SHELL_CANDIDATES`](pwsh → powershell) → `%COMSPEC%` → `cmd.exe`. `SHELL`은 보지 않는다
//!   (Git Bash가 남긴 `/usr/bin/bash` 같은 POSIX 경로라 `CreateProcess`가 못 띄운다).
//! - 환경은 탐색기가 준 사용자 환경 중 [`KEEP_WINDOWS`]와 `PATH`를 그대로 물려준다(로그인 셸 `PATH` 탐침 없음).
//!   변수 이름은 대소문자를 가리지 않는다(`Path`).
//! - 사용자 지정 프로그램은 `PATH`에서 `.exe`·`.cmd` 등을 붙여 절대 경로로 고정한다(`claude` → `claude.cmd`).
//!   `CreateProcess`는 확장자 없는 이름으로 `.cmd`를 찾지 못한다(실측).

use std::process::Stdio;

use krill_proto::os::ffi::{OsStrExt as _, OsStringExt};

use crate::sys::{self, User};

/// Batch trampoline: std's Windows Command handles cmd-specific argv escaping.
#[cfg(windows)]
pub fn run_windows_shell(
    mut args: impl Iterator<Item = std::ffi::OsString>,
) -> std::process::ExitCode {
    use std::io::Write as _;
    let Some(program) = args.next() else {
        return std::process::ExitCode::from(2);
    };
    let result = sys::console_utf8()
        .and_then(|()| sys::protect_agent_launcher())
        .and_then(|()| std::process::Command::new(program).args(args).status());
    match result {
        Ok(status) => {
            std::process::ExitCode::from(u8::try_from(status.code().unwrap_or(1)).unwrap_or(1))
        }
        Err(error) => {
            writeln!(std::io::stderr().lock(), "shell launch failed: {error}").ok();
            std::process::ExitCode::FAILURE
        }
    }
}

/// launchd가 GUI 앱에 주는 PATH(실측, Terminal.app 셸이 받는 값). 나머지는 `/etc/zprofile`의 `path_helper`가 붙인다.
#[cfg(all(unix, not(target_os = "linux")))]
const LAUNCHD_PATH: &[u8] = b"/usr/bin:/bin:/usr/sbin:/sbin";
/// Linux(원격 호스트): Ubuntu `/etc/environment` 기본값. PAM 로그인 없이 `bash -l`만 돌리면 이 값이 안 붙는다.
#[cfg(target_os = "linux")]
const LAUNCHD_PATH: &[u8] = b"/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";
/// Windows 기본 시스템 PATH(추정, 설치 기본값). Windows PowerShell 5.1 폴더를 포함한다.
#[cfg(windows)]
const LAUNCHD_PATH: &[u8] =
    br"C:\Windows\system32;C:\Windows;C:\Windows\System32\WindowsPowerShell\v1.0\";
/// passwd를 못 읽고 `SHELL`도 없을 때의 셸(macOS 기본).
#[cfg(all(unix, not(target_os = "linux")))]
const FALLBACK_SHELL: &[u8] = b"/bin/zsh";
#[cfg(target_os = "linux")]
const FALLBACK_SHELL: &[u8] = b"/bin/sh";
/// Windows 기본 셸 후보(앞이 우선, `PATH`에서 찾는다). 모두 없으면 `%COMSPEC%`, 그것도 없으면 `cmd.exe`.
#[cfg(windows)]
const SHELL_CANDIDATES: &[&str] = &["pwsh.exe", "powershell.exe"];
/// Windows 사용자 환경 중 그대로 물려줄 변수(대소문자 무시). 셸·Node·Git이 홈·임시·시스템 경로를 찾는 데 쓴다.
#[cfg(windows)]
const KEEP_WINDOWS: &[&[u8]] = &[
    b"ALLUSERSPROFILE",
    b"APPDATA",
    b"CommonProgramFiles",
    b"CommonProgramFiles(x86)",
    b"CommonProgramW6432",
    b"COMPUTERNAME",
    b"ComSpec",
    b"DriverData",
    b"HOMEDRIVE",
    b"HOMEPATH",
    b"LOCALAPPDATA",
    b"LOGONSERVER",
    b"NUMBER_OF_PROCESSORS",
    b"OS",
    b"PATHEXT",
    b"PROCESSOR_ARCHITECTURE",
    b"ProgramData",
    b"ProgramFiles",
    b"ProgramFiles(x86)",
    b"ProgramW6432",
    b"PSModulePath",
    b"PROMPT",
    b"PUBLIC",
    b"SystemDrive",
    b"SystemRoot",
    b"TEMP",
    b"TMP",
    b"USERDOMAIN",
    b"USERNAME",
    b"USERPROFILE",
    b"windir",
];
/// `AppleLocale`을 읽지 못했을 때의 LANG. launchd GUI 부모 없이 띄운 개발 셸에서만 나온다.
#[cfg(not(target_os = "linux"))]
const FALLBACK_LANG: &[u8] = b"en_US.UTF-8";
/// Linux는 `en_US` 로케일이 생성돼 있지 않은 이미지가 많다. `C.UTF-8`은 glibc에 늘 있다.
#[cfg(target_os = "linux")]
const FALLBACK_LANG: &[u8] = b"C.UTF-8";
/// 시스템 로케일 조회의 하드 상한. `defaults read` 실측 4~8ms라 여유를 둔다.
const LOCALE_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(200);
/// `defaults read` 출력(`ko_KR`·`ko_KR@...` 등)에서 LANG에 쓸 앞부분만 남길 때의 상한.
const LOCALE_MAX: usize = 32;

/// 로그인 셸 `PATH` 조회의 하드 상한. rc 파일이 멈추거나 백그라운드 자식을 띄워도 데몬이 멈추지 않는다.
#[cfg(unix)]
const PATH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
/// 로그인 셸 stdout 수집 상한.
#[cfg(unix)]
const PATH_MAX: usize = 64 * 1024;
#[cfg(unix)]
const PATH_MARKER: &[u8] = b"__MK_PATH__=";
#[cfg(unix)]
const PATH_PROBE: &str = "/usr/bin/printf '\\n__MK_PATH__='; /usr/bin/printenv PATH";

/// launchd 환경에서 그대로 물려줄 변수. HOME·USER·LOGNAME·SHELL은 passwd에서, PATH는 [`LAUNCHD_PATH`]로 새로 채운다.
/// `XPC_*`·`__CFBundleIdentifier`는 부모 잡을 가리키는 값이라 넘기지 않는다.
const KEEP: &[&[u8]] = &[
    b"TMPDIR",
    b"KRILL_SOCKET_DIR",
    b"SSH_AUTH_SOCK",
    b"__CF_USER_TEXT_ENCODING",
    b"COMMAND_MODE",
    b"LANG",
];

/// 세션 셸 설정.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Shell {
    /// passwd의 로그인 셸을 로그인 모드(`-l`)로. Terminal.app 기본과 같다.
    #[default]
    Login,
    /// 사용자 지정 셸과 인자. 인자는 그대로 넘긴다(로그인 모드가 필요하면 `-l`을 직접 넣는다).
    Custom {
        program: Vec<u8>,
        args: Vec<Vec<u8>>,
    },
}

/// holder `Spawn`에 넣을 argv와 환경 전부.
#[derive(Clone, PartialEq, Eq)]
pub struct Launch {
    pub argv: Vec<Vec<u8>>,
    pub env: Vec<(Vec<u8>, Vec<u8>)>,
}

impl std::fmt::Debug for Launch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // T2: env 값에는 비밀값(OWE-657 프로필 env의 API 키 등)이 들어갈 수 있어
        // 키만 보이고 값은 가린다. argv는 프로그램·인자라 그대로 둔다.
        let env = self
            .env
            .iter()
            .map(|(k, _)| (String::from_utf8_lossy(k), "[redacted]"))
            .collect::<Vec<_>>();
        f.debug_struct("Launch")
            .field("argv", &self.argv)
            .field("env", &env)
            .finish()
    }
}

/// 로그인 셸(`-l -i -c`)이 만든 `PATH`의 성공값만 캐시한다. 실패·타임아웃이면 이번 호출만 기본값을 쓰고 재시도한다.
pub(crate) fn login_path() -> &'static [u8] {
    static PATH: std::sync::OnceLock<Vec<u8>> = std::sync::OnceLock::new();
    cached_path(&PATH, || Shell::Login.launch())
}

fn cached_path(cache: &std::sync::OnceLock<Vec<u8>>, login: impl FnOnce() -> Launch) -> &[u8] {
    if let Some(path) = cache.get() {
        return path;
    }
    if let Some(path) = probe_path(&login()) {
        cache.set(path).ok();
    }
    cache.get().map_or(LAUNCHD_PATH, Vec::as_slice)
}

impl Shell {
    /// 이 프로세스의 환경과 passwd로 argv·env를 만든다.
    ///
    /// 상속값이 없으면 Terminal.app처럼 시스템 로케일(`AppleLocale`)에서 LANG을 만든다
    /// (Terminal.app 실행 상태 측정은 오웬 확인 필요). 읽기에 실패하면 [`FALLBACK_LANG`]이다.
    #[must_use]
    pub fn launch(&self) -> Launch {
        let inherited = std::env::vars_os().map(|(k, v)| (k.into_vec(), v.into_vec()));
        let mut launch = self.launch_with_lang(sys::user().ok().as_ref(), inherited, system_lang());
        if matches!(self, Self::Custom { .. }) {
            set_path(&mut launch, login_path());
        }
        #[cfg(windows)]
        let launch = crate::agent::wrap_windows_shell(launch);
        launch
    }

    #[cfg(test)]
    #[cfg(unix)]
    pub(crate) fn launch_with(
        &self,
        user: Option<&User>,
        inherited: impl IntoIterator<Item = (Vec<u8>, Vec<u8>)>,
    ) -> Launch {
        let inherited: Vec<_> = inherited.into_iter().collect();
        let lang = system_lang();
        let mut launch = self.launch_with_lang(user, inherited.clone(), lang.clone());
        if matches!(self, Self::Custom { .. }) {
            let login = Self::Login.launch_with_lang(user, inherited, lang);
            set_path(
                &mut launch,
                probe_path(&login).as_deref().unwrap_or(LAUNCHD_PATH),
            );
        }
        launch
    }

    /// 테스트용. `system_lang`이 `None`이면 [`FALLBACK_LANG`]을 쓴다.
    pub(crate) fn launch_with_lang(
        &self,
        user: Option<&User>,
        inherited: impl IntoIterator<Item = (Vec<u8>, Vec<u8>)>,
        system_lang: Option<Vec<u8>>,
    ) -> Launch {
        let inherited: Vec<_> = inherited.into_iter().collect();
        let from_env = |key: &[u8]| {
            inherited
                .iter()
                .find(|(k, _)| same_key(k, key))
                .map(|(_, v)| v.clone())
                .filter(|v| !v.is_empty())
        };
        let pick = |pw: Option<&[u8]>, key: &[u8]| {
            pw.filter(|v| !v.is_empty())
                .map(<[u8]>::to_vec)
                .or_else(|| from_env(key))
        };
        let windows_path = if cfg!(windows) {
            let base = from_env(b"PATH");
            krill_proto::os::augment_windows_path(base.as_deref().map(std::ffi::OsStr::from_bytes))
                .map(OsStringExt::into_vec)
                .or(base)
        } else {
            None
        };
        let login_shell = if cfg!(windows) {
            fallback_shell(windows_path.as_deref())
        } else {
            pick(user.map(|u| u.shell.as_slice()), b"SHELL").unwrap_or_else(|| fallback_shell(None))
        };

        let mut env: Vec<(Vec<u8>, Vec<u8>)> =
            inherited.iter().filter(|(k, _)| keep(k)).cloned().collect();
        let has_locale = env
            .iter()
            .any(|(k, _)| matches!(k.as_slice(), b"LANG" | b"LC_ALL" | b"LC_CTYPE"));
        if !has_locale {
            let lang = system_lang.unwrap_or_else(|| FALLBACK_LANG.to_vec());
            env.push((b"LANG".to_vec(), lang));
        }
        for (key, value) in [
            (&b"HOME"[..], pick(user.map(|u| u.home.as_slice()), b"HOME")),
            (b"USER", pick(user.map(|u| u.name.as_slice()), b"USER")),
            (
                b"LOGNAME",
                pick(user.map(|u| u.name.as_slice()), b"LOGNAME"),
            ),
        ] {
            if let Some(v) = value {
                env.push((key.to_vec(), v));
            }
        }
        env.extend([
            (b"SHELL".to_vec(), login_shell.clone()),
            (
                b"PATH".to_vec(),
                // Windows는 로그인 셸 탐침이 없어 물려받은 PATH에 사용자 CLI 기본 폴더를 보강한 값이 기준이다.
                windows_path
                    .clone()
                    .unwrap_or_else(|| LAUNCHD_PATH.to_vec()),
            ),
            (b"TERM".to_vec(), b"xterm-256color".to_vec()),
            (b"COLORTERM".to_vec(), b"truecolor".to_vec()),
        ]);

        let argv = match self {
            // pwsh·powershell·cmd에는 `-l`이 없다.
            Self::Login if cfg!(windows) => vec![login_shell],
            Self::Login => vec![login_shell, b"-l".to_vec()],
            Self::Custom { program, args } => {
                std::iter::once(resolve(program, windows_path.as_deref()))
                    .chain(args.iter().cloned())
                    .collect()
            }
        };
        #[cfg(windows)]
        let argv = windows_interactive(argv, &mut env);
        Launch { argv, env }
    }
}

/// Only implicit interactive starts are bootstrapped; explicit -File/-Command/-c
/// invocations retain their arguments and execute exactly once.
#[cfg(any(windows, test))]
fn windows_interactive(mut argv: Vec<Vec<u8>>, env: &mut Vec<(Vec<u8>, Vec<u8>)>) -> Vec<Vec<u8>> {
    if argv.len() != 1 {
        return argv;
    }
    let Some(program) = argv.first().and_then(|p| std::str::from_utf8(p).ok()) else {
        return argv;
    };
    let name = program
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(program)
        .to_ascii_lowercase();
    match name.strip_suffix(".exe").unwrap_or(&name) {
        "powershell" | "pwsh" => {
            // PowerShell loads profiles once before -Command. Preserve their prompt
            // and native pipeline encoding while reporting filesystem cwd via OSC 9;9.
            const BOOTSTRAP: &str = concat!(
                "$global:OutputEncoding=[Console]::InputEncoding=[Console]::OutputEncoding=New-Object System.Text.UTF8Encoding $false;",
                "$global:__KrillOriginalPrompt=$function:prompt;",
                "function global:prompt { ",
                "$p=$executionContext.SessionState.Path.CurrentFileSystemLocation.Path -replace '[\\x00-\\x1f\\x7f-\\x9f]','';",
                "[Console]::Write(([string][char]27)+']9;9;\"'+$p+'\"'+[char]7);",
                "if($global:__KrillOriginalPrompt){ & $global:__KrillOriginalPrompt } else { 'PS '+$pwd+'> ' } }"
            );
            argv.extend([
                b"-NoExit".to_vec(),
                b"-Command".to_vec(),
                BOOTSTRAP.as_bytes().to_vec(),
            ]);
        }
        "cmd" => {
            let prompt = env
                .iter()
                .find(|(key, _)| key.eq_ignore_ascii_case(b"PROMPT"))
                .map_or_else(|| b"$P$G".to_vec(), |(_, value)| value.clone());
            env.retain(|(key, _)| !key.eq_ignore_ascii_case(b"PROMPT"));
            let mut integrated = b"$e]9;9;\"$P\"$e\\".to_vec();
            integrated.extend(prompt);
            env.push((b"PROMPT".to_vec(), integrated));
            argv.extend([b"/K".to_vec(), b"chcp 65001 >nul".to_vec()]);
        }
        _ => {}
    }
    argv
}

/// 환경 변수 이름 비교. Windows는 대소문자를 가리지 않는다.
fn same_key(a: &[u8], b: &[u8]) -> bool {
    if cfg!(windows) {
        a.eq_ignore_ascii_case(b)
    } else {
        a == b
    }
}

#[cfg(unix)]
fn keep(key: &[u8]) -> bool {
    KEEP.contains(&key) || key.starts_with(b"LC_")
}

#[cfg(windows)]
fn keep(key: &[u8]) -> bool {
    KEEP.iter().chain(KEEP_WINDOWS).any(|k| same_key(k, key)) || key.starts_with(b"LC_")
}

#[cfg(unix)]
fn fallback_shell(_path: Option<&[u8]>) -> Vec<u8> {
    FALLBACK_SHELL.to_vec()
}

/// `path`(물려받은 PATH)에서 [`SHELL_CANDIDATES`]를 찾는다. 없으면 `%COMSPEC%`, 그것도 없으면 `cmd.exe`.
#[cfg(windows)]
fn fallback_shell(path: Option<&[u8]>) -> Vec<u8> {
    use krill_proto::os::ffi::OsStrExt as _;
    let path = std::ffi::OsStr::from_bytes(path.unwrap_or_default());
    SHELL_CANDIDATES
        .iter()
        .find_map(|c| krill_proto::agent::which(c, path))
        .map(std::path::PathBuf::into_os_string)
        .or_else(|| std::env::var_os("COMSPEC"))
        .map_or_else(|| b"cmd.exe".to_vec(), OsStringExt::into_vec)
}

/// Unix는 프로그램 이름을 그대로 둔다(holder의 `execvp`가 PATH를 찾는다).
#[cfg(unix)]
fn resolve(program: &[u8], _path: Option<&[u8]>) -> Vec<u8> {
    program.to_vec()
}

/// Windows: `path`에서 확장자까지 찾아 절대 경로로 고정한다(`claude` → `…\claude.cmd`). 못 찾으면 그대로 둔다
/// (holder 실행이 실패로 알린다).
#[cfg(windows)]
fn resolve(program: &[u8], path: Option<&[u8]>) -> Vec<u8> {
    use krill_proto::os::ffi::OsStrExt as _;
    std::str::from_utf8(program)
        .ok()
        .and_then(|p| krill_proto::agent::which(p, std::ffi::OsStr::from_bytes(path?)))
        .map_or_else(|| program.to_vec(), |p| p.into_os_string().into_vec())
}

/// Terminal.app처럼 시스템 로케일(`AppleLocale`→`ko_KR`)에서 LANG(`ko_KR.UTF-8`)을 만든다.
///
/// 못 읽으면 `None`(호출자가 [`FALLBACK_LANG`]을 쓴다). `None`은 `AppleLocale`이 없을 때와
/// 읽기·파싱 실패뿐이라, 결과는 "시스템 로케일 LANG" 아니면 "읽기 실패" 둘 중 하나다.
fn system_lang() -> Option<Vec<u8>> {
    lang_from_locale(&system_locale()?)
}

/// ko_KR→ko_KR.UTF-8. @ 뒤(배리언트)와 . 뒤(기존 인코딩)는 버린다.
/// 소문자는 언어를, 대문자는 지역을 뜻하므로(예: `ko_KR`) 그대로 둔다.
fn lang_from_locale(locale: &[u8]) -> Option<Vec<u8>> {
    let mut base: Vec<u8> = Vec::new();
    for b in locale {
        if *b == b'@' || *b == b'.' {
            break;
        }
        base.push(*b);
    }
    if base.is_empty() || base.len() > LOCALE_MAX {
        return None;
    }
    let ok = base
        .iter()
        .all(|b| b.is_ascii_alphanumeric() || *b == b'_' || *b == b'-');
    if !ok {
        return None;
    }
    base.extend_from_slice(b".UTF-8");
    Some(base)
}

/// `AppleLocale`을 본다. krilld 같은 자식은 launchd GUI 환경을 물려받아 이미 갖고 있다.
/// 부모에 없을 때만 사용자 기본 설정에서 defaults read로 읽는다(200ms 상한, 실패는 None).
fn system_locale() -> Option<Vec<u8>> {
    if let Some(v) = std::env::var_os("AppleLocale") {
        let v = v.into_vec();
        if !v.is_empty() {
            return Some(v);
        }
    }
    defaults_locale()
}

/// defaults read -g AppleLocale(출력이 없으면 NSGlobalDomain)을 짧게 돌린다.
/// plist 직접 읽기와 `CFPreferences`는 의존성·파서가 필요해 쓰지 않는다(ponytail: std+설치된 도구).
fn defaults_locale() -> Option<Vec<u8>> {
    for domain in ["-g", "NSGlobalDomain"] {
        if let Some(v) = defaults_read(domain) {
            return Some(v);
        }
    }
    None
}

fn defaults_read(domain: &str) -> Option<Vec<u8>> {
    let mut child = std::process::Command::new("/usr/bin/defaults")
        .args(["read", domain, "AppleLocale"])
        .stdin(Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let status = child.wait_timeout(LOCALE_TIMEOUT)?;
    let mut out = Vec::new();
    if let Some(mut stdout) = child.stdout.take() {
        use std::io::Read;
        stdout.read_to_end(&mut out).ok()?;
    }
    child.wait().ok();
    if !status.success() {
        return None;
    }
    let trimmed: Vec<u8> = trim_ascii(&out);
    if trimmed.is_empty() {
        return None;
    }
    Some(trimmed)
}

/// wait에 200ms 상한을 두기 위한 최소 구현. 자식은 출력이 한 줄이라 파이프가 막히지 않는다.
/// 출력이 크면 상한까지만 기다리고 `None`이다(호출자는 [`FALLBACK_LANG`]을 쓴다).
trait WaitTimeout {
    fn wait_timeout(&mut self, timeout: std::time::Duration) -> Option<std::process::ExitStatus>;
}

impl WaitTimeout for std::process::Child {
    fn wait_timeout(&mut self, timeout: std::time::Duration) -> Option<std::process::ExitStatus> {
        let start = std::time::Instant::now();
        loop {
            if let Ok(Some(status)) = self.try_wait() {
                return Some(status);
            }
            if start.elapsed() >= timeout {
                self.kill().ok();
                self.wait().ok();
                return None;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }
}

fn trim_ascii(out: &[u8]) -> Vec<u8> {
    let is_space = |b: &u8| matches!(*b, b' ' | b'\t' | b'\n' | b'\r');
    let bytes: Vec<u8> = out.to_vec();
    let mut front: usize = 0;
    for b in &bytes {
        if is_space(b) {
            front += 1;
        } else {
            break;
        }
    }
    let mut back: usize = 0;
    for b in bytes.iter().rev() {
        if is_space(b) {
            back += 1;
        } else {
            break;
        }
    }
    let keep = bytes.len().saturating_sub(front.saturating_add(back));
    bytes.into_iter().skip(front).take(keep).collect()
}

fn set_path(launch: &mut Launch, path: &[u8]) {
    if let Some((_, v)) = launch.env.iter_mut().find(|(k, _)| k.as_slice() == b"PATH") {
        *v = path.to_vec();
    }
}

/// 에이전트 `wrap`과 같은 방식(`<login_shell> -l -i -c`)으로 로그인 셸의 `PATH`를 읽는다.
#[cfg(unix)]
fn probe_path(login: &Launch) -> Option<Vec<u8>> {
    use std::ffi::OsStr;
    use std::io::Read as _;
    use std::os::unix::ffi::OsStrExt as _;

    let program = login.argv.first()?;
    let Ok(mut child) = std::process::Command::new(OsStr::from_bytes(program))
        .args(["-l", "-i", "-c", PATH_PROBE])
        .env_clear()
        .envs(
            login
                .env
                .iter()
                .map(|(k, v)| (OsStr::from_bytes(k), OsStr::from_bytes(v))),
        )
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    else {
        return None;
    };
    let Some(mut stdout) = child.stdout.take() else {
        kill_probe(&mut child);
        return None;
    };
    if let Ok(flags) = rustix::fs::fcntl_getfl(&stdout) {
        rustix::fs::fcntl_setfl(&stdout, flags | rustix::fs::OFlags::NONBLOCK).ok();
    }
    let mut out = Vec::new();
    let mut buf = [0u8; 4096];
    let start = std::time::Instant::now();
    let status = loop {
        while out.len() < PATH_MAX {
            match stdout.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => out.extend(
                    buf.iter()
                        .take(n)
                        .take(PATH_MAX.saturating_sub(out.len()))
                        .copied(),
                ),
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                    ) =>
                {
                    break;
                }
                Err(_) => break,
            }
        }
        if let Ok(Some(status)) = child.try_wait() {
            while out.len() < PATH_MAX {
                match stdout.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => out.extend(
                        buf.iter()
                            .take(n)
                            .take(PATH_MAX.saturating_sub(out.len()))
                            .copied(),
                    ),
                }
            }
            break Some(status);
        }
        if start.elapsed() >= PATH_TIMEOUT {
            break None;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    };
    kill_probe(&mut child);
    if !status.is_some_and(|s| s.success()) {
        return None;
    }
    extract_path(&out)
}

/// Windows는 로그인 셸 탐침이 없다. 셸에 줄 PATH(물려받은 값)가 곧 답이다.
#[cfg(windows)]
fn probe_path(login: &Launch) -> Option<Vec<u8>> {
    login
        .env
        .iter()
        .find(|(k, _)| k.as_slice() == b"PATH")
        .map(|(_, v)| v.clone())
        .filter(|v| !v.is_empty())
}

#[cfg(unix)]
fn extract_path(out: &[u8]) -> Option<Vec<u8>> {
    out.split(|&b| b == b'\n')
        .rev()
        .find_map(|line| {
            let line = line.strip_suffix(b"\r").unwrap_or(line);
            line.strip_prefix(PATH_MARKER)
        })
        .filter(|p| !p.is_empty() && !p.contains(&0))
        .map(<[u8]>::to_vec)
}

#[cfg(unix)]
fn kill_probe(child: &mut std::process::Child) {
    child.kill().ok();
    child.wait().ok();
}

#[cfg(test)]
#[cfg(unix)]
mod tests;

#[cfg(test)]
#[cfg(windows)]
mod windows_tests {
    use super::*;

    fn inherited(path: &std::path::Path) -> Vec<(Vec<u8>, Vec<u8>)> {
        let mut env: Vec<_> = std::env::vars_os()
            .map(|(k, v)| (k.into_encoded_bytes(), v.into_encoded_bytes()))
            .filter(|(k, _)| !k.eq_ignore_ascii_case(b"PATH"))
            .collect();
        let system = std::env::var_os("PATH").unwrap_or_default();
        let mut joined = path.as_os_str().to_owned();
        joined.push(";");
        joined.push(system);
        env.push((b"Path".to_vec(), joined.into_encoded_bytes()));
        env
    }

    #[test]
    fn login_shell_env_and_cmd_resolution() {
        let dir = std::env::temp_dir().join(format!("krilld-shell 한글-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("fake-agent.cmd"), "@echo off\r\n").unwrap();
        let env = inherited(&dir);

        let login = Shell::Login.launch_with_lang(None, env.clone(), None);
        let shell = String::from_utf8(login.argv[0].clone()).unwrap();
        eprintln!("login shell: {shell}");
        assert_eq!(login.argv.len(), 1);
        assert!(
            std::path::Path::new(&shell).is_absolute() || shell.eq_ignore_ascii_case("cmd.exe")
        );
        let get = |k: &[u8]| {
            login
                .env
                .iter()
                .find(|(key, _)| key.eq_ignore_ascii_case(k))
        };
        assert!(get(b"SystemRoot").is_some());
        assert!(get(b"USERPROFILE").is_some());
        let path = get(b"PATH").unwrap();
        assert_eq!(path.0, b"PATH");
        assert!(path.1.starts_with(dir.as_os_str().as_encoded_bytes()));

        let custom = Shell::Custom {
            program: b"fake-agent".to_vec(),
            args: vec![b"has space".to_vec()],
        }
        .launch_with_lang(None, env.clone(), None);
        assert_eq!(
            custom.argv[0],
            dir.join("fake-agent.cmd")
                .into_os_string()
                .into_encoded_bytes()
        );
        assert_eq!(custom.argv[1], b"has space");
        let missing = Shell::Custom {
            program: b"no-such-agent".to_vec(),
            args: Vec::new(),
        }
        .launch_with_lang(None, env, None);
        assert_eq!(missing.argv[0], b"no-such-agent");
        std::fs::remove_dir_all(dir).ok();
    }
}

#[cfg(test)]
mod windows_bootstrap_tests {
    use super::windows_interactive;

    #[test]
    fn interactive_bootstrap_preserves_profiles_prompt_and_explicit_commands() {
        let mut env = vec![(b"PrOmPt".to_vec(), b"custom $P$G".to_vec())];
        let cmd = windows_interactive(vec![br"C:\Windows\System32\cmd.exe".to_vec()], &mut env);
        assert_eq!(&cmd[1..], &[b"/K".to_vec(), b"chcp 65001 >nul".to_vec()]);
        assert_eq!(
            env,
            vec![(b"PROMPT".to_vec(), b"$e]9;9;\"$P\"$e\\custom $P$G".to_vec())]
        );
        let ps = windows_interactive(vec![b"powershell.exe".to_vec()], &mut Vec::new());
        assert_eq!(&ps[1..3], &[b"-NoExit".to_vec(), b"-Command".to_vec()]);
        let bootstrap = String::from_utf8(ps[3].clone()).unwrap();
        assert!(bootstrap.contains("[Console]::InputEncoding"));
        assert!(bootstrap.contains("$global:__KrillOriginalPrompt"));
        assert!(
            bootstrap.contains("[char]27"),
            "PowerShell 5.1 has no backtick-e"
        );
        assert!(!bootstrap.contains("$PROFILE"));
        for program in ["pwsh.exe", "powershell.exe", "cmd.exe", "bash.exe"] {
            let explicit = vec![
                program.as_bytes().to_vec(),
                b"-c".to_vec(),
                b"echo hi".to_vec(),
            ];
            assert_eq!(
                windows_interactive(explicit.clone(), &mut Vec::new()),
                explicit
            );
        }
    }

    /// Agents started from the default PowerShell (agy runs `powershell -NoProfile` with piped
    /// stdout) inherit the console code page. At the Korean default 949 their tool output is not
    /// UTF-8, so the bootstrap must switch the console itself, not only PowerShell's variables.
    #[cfg(windows)]
    #[test]
    fn powershell_bootstrap_keeps_noprofile_child_pipe_output_utf8_under_codepage_949() {
        use std::os::windows::process::CommandExt as _;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let root = std::env::temp_dir().join(format!("krill-shell-cp-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let script = root.join("emit.ps1");
        std::fs::write(
            &script,
            "Write-Output ([string][char]0xD55C + [char]0xAE00 + '-notion.md')\r\n",
        )
        .unwrap();
        let mut argv = windows_interactive(vec![b"powershell.exe".to_vec()], &mut Vec::new());
        assert_eq!(argv.remove(1), b"-NoExit");
        let bootstrap = String::from_utf8(argv.pop().unwrap()).unwrap();
        // A fresh hidden console stands in for the session ConPTY at its Korean default.
        let output = std::process::Command::new("powershell.exe")
            .creation_flags(CREATE_NO_WINDOW)
            .args(["-NoProfile", "-Command"])
            .arg(format!(
                "chcp.com 949 > $null; {bootstrap}; & powershell -NoProfile -File '{}'",
                script.display()
            ))
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        assert_eq!(
            String::from_utf8(output.stdout).as_deref(),
            Ok("한글-notion.md\r\n")
        );
        std::fs::remove_dir_all(root).ok();
    }
}

#[cfg(test)]
mod socket_environment_tests {
    use super::*;

    #[test]
    fn socket_override_survives_filtered_shell_environment() {
        let inherited = vec![
            (
                b"KRILL_SOCKET_DIR".to_vec(),
                b"private-socket-root".to_vec(),
            ),
            (b"UNRELATED_SECRET".to_vec(), b"do-not-inherit".to_vec()),
        ];
        let launch = Shell::Login.launch_with_lang(None, inherited, None);
        assert!(launch.env.contains(&(
            b"KRILL_SOCKET_DIR".to_vec(),
            b"private-socket-root".to_vec()
        )));
        assert!(!launch.env.iter().any(|(key, _)| key == b"UNRELATED_SECRET"));
        #[cfg(windows)]
        assert!(keep(b"krill_socket_dir"));
    }
}
