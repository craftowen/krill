//! OWE-897: 관리형 원격 서버(빈 SSH 호스트에 krill 설치·기동·업데이트).
//!
//! - 원격 OS/아키텍처 감지(`uname -s -m` 한 번 조회): macOS(`arm64`)만 지원,
//!   Linux는 미지원으로 보고한다(원격 데몬 경로·`launchd`가 macOS 전제).
//! - 원격 krill 버전 조회(`~/.krill/bin/VERSION`, 없으면 PATH의 `krill` 유무만 보고).
//! - 설치는 로컬에서 검증된 바이너리 바이트를 SSH stdin으로 전송한 뒤 원격에서
//!   `~/.krill/bin` 배치·`VERSION` 기록·`launchd` 잡 등록만 수행한다.
//!   원격 사용자 파일·셸 설정(`~/.zshrc` 등)은 절대 건드리지 않는다.
//! - 모든 원격 호출은 [`crate::ssh::SshOptions`] argv 빌더 + 주입 가능한 `ssh` 실행 파일로
//!   가짜 SSH 스크립트에 테스트 가능하다(실제 네트워크 금지).

use std::ffi::OsStr;
use std::io;
#[cfg(unix)]
use std::io::Write as _;
use std::path::PathBuf;
#[cfg(unix)]
use std::process::{Command, Stdio};

use krill_proto::control::is_newer;
use krill_proto::daemon::{self, HOLDER_LABEL};

use crate::ssh::{self, SshOptions};

/// 관리형 설치 디렉터리(원격 `$HOME` 아래, OWE-897).
pub const REMOTE_BIN_DIR: &str = ".krill/bin";
/// 관리형 설치 버전 기록 파일(원격 `$HOME/.krill/bin/VERSION`).
pub const REMOTE_VERSION_FILE: &str = "VERSION";
/// 전송하는 로컬 바이너리 이름(원격 `~/.krill/bin`에 그대로 배치).
pub const MANAGED_BINARIES: [&str; 3] = ["krill", "krilld", "krill-holder"];
/// `launchd` 잡 등록 실패 등 원격 셸 스크립트의 단계별 종료 코드.
const EXIT_UNSUPPORTED_OS: i32 = 3;
const EXIT_MISSING_BLOB: i32 = 4;
const EXIT_MISSING_PLIST: i32 = 5;
const EXIT_BOOTSTRAP_FAILED: i32 = 6;

/// 원격 OS.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoteOs {
    Macos,
    Linux,
    /// `krill __bridge`로 붙는 Windows 원격. 관리형 설치는 하지 않는다(그 PC에서 설치·업데이트).
    Windows,
    Other(String),
}

/// 원격 CPU 아키텍처.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoteArch {
    Arm64,
    X86_64,
    Other(String),
}

impl RemoteArch {
    fn label(&self) -> String {
        match self {
            Self::Arm64 => "arm64".to_owned(),
            Self::X86_64 => "x86_64".to_owned(),
            Self::Other(raw) => raw.clone(),
        }
    }
}

/// `uname -s` 파싱(macOS `Darwin`, Linux, 그 외 원문 보관).
#[must_use]
pub fn parse_os(raw: &str) -> RemoteOs {
    let trimmed = raw.trim();
    if trimmed.eq_ignore_ascii_case("darwin") {
        RemoteOs::Macos
    } else if trimmed.eq_ignore_ascii_case("linux") {
        RemoteOs::Linux
    } else if trimmed.eq_ignore_ascii_case("windows") {
        RemoteOs::Windows
    } else {
        RemoteOs::Other(trimmed.chars().take(32).collect())
    }
}

/// `uname -m` 파싱(`arm64`·`aarch64`, `x86_64`·`amd64`, 그 외 원문 보관).
#[must_use]
pub fn parse_arch(raw: &str) -> RemoteArch {
    let lowered = raw.trim().to_ascii_lowercase();
    match lowered.as_str() {
        "arm64" | "aarch64" => RemoteArch::Arm64,
        "x86_64" | "amd64" => RemoteArch::X86_64,
        _ => RemoteArch::Other(raw.trim().chars().take(32).collect()),
    }
}

/// 원격 플랫폼.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemotePlatform {
    pub os: RemoteOs,
    pub arch: RemoteArch,
    /// `uname -s` 원문(표시·디버그용).
    pub kernel: String,
}

impl RemotePlatform {
    /// macOS(`arm64`)만 관리형 설치를 지원한다.
    /// Linux는 원격 데몬 경로·`launchd` 전제 때문에 미지원으로 보고한다(OWE-897).
    #[must_use]
    pub fn is_supported(&self) -> bool {
        matches!(self.os, RemoteOs::Macos) && matches!(self.arch, RemoteArch::Arm64)
    }

    /// UI 표시용 (`macOS arm64` 등).
    #[must_use]
    pub fn describe(&self) -> String {
        let os_label = match &self.os {
            RemoteOs::Macos => "macOS".to_owned(),
            RemoteOs::Linux => "Linux".to_owned(),
            RemoteOs::Windows => "Windows".to_owned(),
            RemoteOs::Other(raw) => {
                if raw.is_empty() {
                    "알 수 없는 OS".to_owned()
                } else {
                    raw.clone()
                }
            }
        };
        format!("{} {}", os_label, self.arch.label())
    }

    /// 로컬 바이너리를 그대로 전송해 실행할 수 있는지.
    /// Apple Silicon Mac용 로컬 바이너리만 Apple Silicon 원격에 전송한다.
    #[must_use]
    pub fn accepts_local_arch(&self, local_arch: &str) -> bool {
        if !self.is_supported() {
            return false;
        }
        local_arch == "aarch64"
    }
}

/// 원격 krill 설치 버전 상태.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoteVersion {
    /// `~/.krill/bin/VERSION`에 기록된 버전.
    Known(String),
    /// PATH에는 `krill`이 있으나 관리형 설치가 아니라 버전 모름.
    Unmanaged,
    /// `krill` 없음.
    Missing,
}

/// 로컬 버전과 비교한 원격 버전 상태.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VersionStatus {
    UpToDate,
    UpdateAvailable { remote: String, local: String },
    NotInstalled,
    Unknown,
}

/// 원격 프로브 결과(OS·아키·홈·버전·소켓 번들).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteInfo {
    pub platform: RemotePlatform,
    pub home: String,
    pub version: RemoteVersion,
    pub socket_bundle: Option<String>,
}

impl RemoteInfo {
    /// 원격 `~/.krill/bin` 절대 경로.
    #[must_use]
    pub fn bin_dir(&self) -> PathBuf {
        PathBuf::from(format!("{}/{REMOTE_BIN_DIR}", self.home))
    }

    /// `VERSION` 파일 원격 경로.
    #[must_use]
    pub fn version_file(&self) -> PathBuf {
        PathBuf::from(format!(
            "{}/{REMOTE_BIN_DIR}/{REMOTE_VERSION_FILE}",
            self.home
        ))
    }

    /// 원격 krilld 소켓 경로(프로브의 소켓 번들, 없으면 새 번들).
    #[must_use]
    pub fn socket_path(&self) -> PathBuf {
        let bundle = self.socket_bundle.as_deref().unwrap_or(daemon::BUNDLE_ID);
        PathBuf::from(format!(
            "{}/Library/Application Support/{bundle}/run/krilld.sock",
            self.home
        ))
    }

    /// 원격 `LaunchAgents` plist 경로.
    #[must_use]
    pub fn plist_path(&self) -> PathBuf {
        PathBuf::from(format!(
            "{}/Library/LaunchAgents/{HOLDER_LABEL}.plist",
            self.home
        ))
    }

    #[must_use]
    pub fn version_status(&self, local_version: &str) -> VersionStatus {
        match &self.version {
            RemoteVersion::Missing => VersionStatus::NotInstalled,
            RemoteVersion::Unmanaged => VersionStatus::Unknown,
            RemoteVersion::Known(remote) => match is_newer(local_version, remote) {
                Some(true) => VersionStatus::UpdateAvailable {
                    remote: remote.clone(),
                    local: local_version.to_owned(),
                },
                Some(false) => VersionStatus::UpToDate,
                None => VersionStatus::Unknown,
            },
        }
    }
}

/// 원격 프로브 셸 스크립트(`/bin/sh -c '…'` 한 번 호출, fish·tcsh 로그인 셸 안전).
/// `krill-os`·`krill-arch`·`krill-home`·`krill-version`·`krill-sock` 라벨 행만 출력한다.
pub fn probe_command() -> String {
    format!(
        "/bin/sh -c 'printf \"krill-os=%s\\n\" \"$(uname -s)\"; printf \"krill-arch=%s\\n\" \"$(uname -m)\"; printf \"krill-home=%s\\n\" \"$HOME\"; if [ -f \"$HOME/{REMOTE_BIN_DIR}/{REMOTE_VERSION_FILE}\" ]; then printf \"krill-version=%s\\n\" \"$(cat \"$HOME/{REMOTE_BIN_DIR}/{REMOTE_VERSION_FILE}\" 2>/dev/null)\"; elif command -v krill >/dev/null 2>&1; then printf \"krill-version=%s\\n\" \"unmanaged\"; else printf \"krill-version=%s\\n\" \"missing\"; fi; b={}; if [ -S \"$HOME/Library/Application Support/$b/run/krilld.sock\" ]; then printf \"krill-sock=%s\\n\" \"$b\"; fi'",
        daemon::BUNDLE_ID
    )
}

/// `VERSION` 파일 내용 검증(첫 줄, 64자 이내, 제어문자·`=`·`:` 거부).
fn parse_version_value(raw: &str) -> Option<String> {
    let first = raw.lines().next().unwrap_or_default().trim();
    if first.is_empty() || first.len() > 64 {
        return None;
    }
    if first == "missing" || first == "unmanaged" {
        return None;
    }
    let ok = first
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '+'));
    ok.then(|| first.to_owned())
}

/// [`probe_command`] 출력 파싱. 라벨 없는 행(MOTD 등)은 무시한다.
pub fn parse_probe(output: &str) -> io::Result<RemoteInfo> {
    let mut os: Option<String> = None;
    let mut arch: Option<String> = None;
    let mut home: Option<String> = None;
    let mut version: Option<String> = None;
    let mut sock: Option<String> = None;
    for line in output.lines() {
        let (key, value) = line.split_once('=').unwrap_or(("", ""));
        match key {
            "krill-os" => os = Some(value.to_owned()),
            "krill-arch" => arch = Some(value.to_owned()),
            "krill-home" => home = Some(value.to_owned()),
            "krill-version" => version = Some(value.to_owned()),
            "krill-sock" => sock = Some(value.to_owned()),
            _ => {}
        }
    }
    let (Some(os_raw), Some(arch_raw), Some(home_raw)) = (os, arch, home) else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("remote probe is missing os/arch/home lines: {output:?}"),
        ));
    };
    if !ssh::valid_remote_socket(&home_raw) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("remote probe has invalid HOME: {home_raw:?}"),
        ));
    }
    let version = match version.as_deref().map(str::trim) {
        None | Some("missing") => RemoteVersion::Missing,
        Some("unmanaged") => RemoteVersion::Unmanaged,
        Some(raw) => match parse_version_value(raw) {
            Some(known) => RemoteVersion::Known(known),
            None => RemoteVersion::Unmanaged,
        },
    };
    let socket_bundle = sock
        .map(|s| s.trim().to_owned())
        .filter(|s| *s == daemon::BUNDLE_ID);
    Ok(RemoteInfo {
        platform: RemotePlatform {
            os: parse_os(&os_raw),
            arch: parse_arch(&arch_raw),
            kernel: os_raw.trim().chars().take(32).collect(),
        },
        home: home_raw,
        version,
        socket_bundle,
    })
}

/// `ssh <opts> <probe>` 한 번 실행해 [`RemoteInfo`]를 조회한다.
pub fn query_remote_info(opts: &SshOptions) -> io::Result<RemoteInfo> {
    query_remote_info_with_program(opts, &ssh::ssh_program())
}

fn query_remote_info_with_program(opts: &SshOptions, ssh_bin: &OsStr) -> io::Result<RemoteInfo> {
    let out = run_ssh(opts, ssh_bin, &opts.probe_args(), None)?;
    if !out.status.success() {
        // POSIX 프로브가 원격에서 실패했다(Windows의 cmd.exe 등). 원격 krill 중계로 다시 묻는다.
        if ssh::bridge::reached_remote(out.status)
            && let Some(probe) = ssh::bridge::probe(opts, ssh_bin)?
        {
            return Ok(bridge_info(&probe));
        }
        return Err(probe_failed(opts, &out));
    }
    let text =
        String::from_utf8(out.stdout).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    parse_probe(&text)
}

/// `krill __bridge --probe` 답을 [`RemoteInfo`]로 옮긴다. 원격 `HOME`은 묻지 않는다(관리형 설치 대상이 아니다).
fn bridge_info(probe: &ssh::Probe) -> RemoteInfo {
    RemoteInfo {
        platform: RemotePlatform {
            os: parse_os(&probe.os),
            arch: parse_arch(&probe.arch),
            kernel: probe.os.chars().take(32).collect(),
        },
        home: String::new(),
        version: parse_version_value(&probe.version)
            .map_or(RemoteVersion::Unmanaged, RemoteVersion::Known),
        socket_bundle: None,
    }
}

fn probe_failed(opts: &SshOptions, out: &std::process::Output) -> io::Error {
    let stderr = String::from_utf8_lossy(&out.stderr);
    let detail = stderr.trim();
    io::Error::other(if detail.is_empty() {
        format!(
            "remote probe to {} failed ({})",
            opts.destination, out.status
        )
    } else {
        format!("remote probe to {} failed: {detail}", opts.destination)
    })
}

/// 셸 단일 인용부호로 감싼다(`'` → `'\''`).
fn shell_quote(s: &str) -> String {
    if s.bytes().all(|b| {
        b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'@' | b'/' | b':' | b'+')
    }) {
        return s.to_owned();
    }
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// 버전 문자열 검증(설치 스크립트에 그대로 박히므로 엄격: 영숫자·`._+-`만, 64자 이내).
fn valid_version(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '+' | '-'))
}

/// 관리형 설치 명세(원격 `$HOME`·버전·plist).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallSpec {
    pub home: String,
    pub version: String,
}

impl InstallSpec {
    pub fn new(home: &str, version: &str) -> io::Result<Self> {
        if !ssh::valid_remote_socket(home) || home.contains('\'') {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("invalid remote HOME: {home:?}"),
            ));
        }
        if !valid_version(version) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("invalid version: {version:?}"),
            ));
        }
        Ok(Self {
            home: home.to_owned(),
            version: version.to_owned(),
        })
    }

    #[must_use]
    pub fn bin_dir(&self) -> String {
        format!("{}/{REMOTE_BIN_DIR}", self.home)
    }

    #[must_use]
    pub fn plist_path(&self) -> String {
        format!("{}/Library/LaunchAgents/{HOLDER_LABEL}.plist", self.home)
    }

    /// 원격 holder plist XML(로컬 순수 함수, [`daemon::holder_plist`] 재사용).
    #[must_use]
    pub fn render_plist(&self) -> String {
        let bin = self.bin_dir();
        daemon::holder_plist(
            &format!("{bin}/krill-holder"),
            &format!("{bin}/krilld"),
            &format!(
                "{}/Library/Application Support/{}/run/krilld.sock",
                self.home,
                daemon::BUNDLE_ID
            ),
        )
    }

    /// 원격 최종 설정 스크립트(stdin으로 `/bin/sh -s`에 흘림).
    /// 하는 일: `uname` 가드 → `chmod` → `VERSION` 기록 → `launchctl` 재등록.
    /// 셸 설정 파일(`~/.zshrc` 등)은 절대 건드리지 않는다.
    #[must_use]
    pub fn render_setup_script(&self) -> String {
        let bin = shell_quote(&self.bin_dir());
        let plist = shell_quote(&self.plist_path());
        let version = shell_quote(&self.version);
        format!(
            r#"set -u
if [ "$(uname -s)" != "Darwin" ]; then echo "krill-install: unsupported OS: $(uname -s)" >&2; exit {EXIT_UNSUPPORTED_OS}; fi
BIN={bin}
if [ ! -d "$BIN" ]; then echo "krill-install: missing bin dir $BIN" >&2; exit {EXIT_MISSING_BLOB}; fi
for b in krill krilld krill-holder; do
  if [ ! -f "$BIN/$b" ]; then echo "krill-install: missing $BIN/$b" >&2; exit {EXIT_MISSING_BLOB}; fi
  chmod 755 "$BIN/$b"
done
printf '%s\n' {version} > "$BIN/{REMOTE_VERSION_FILE}"
chmod 644 "$BIN/{REMOTE_VERSION_FILE}"
PLIST={plist}
if [ ! -f "$PLIST" ]; then echo "krill-install: missing plist $PLIST" >&2; exit {EXIT_MISSING_PLIST}; fi
UID_NUM="$(id -u)"
launchctl bootout "gui/$UID_NUM/{HOLDER_LABEL}" >/dev/null 2>&1
if ! BOOT_ERR="$(launchctl bootstrap "gui/$UID_NUM" "$PLIST" 2>&1)"; then echo "krill-install: launchctl bootstrap failed: $BOOT_ERR" >&2; exit {EXIT_BOOTSTRAP_FAILED}; fi
echo "krill-install: ok {version}"
"#
        )
    }
}

/// 설치 실패 원인(설정 화면의 실패 안내 매핑용).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallFailureKind {
    /// SSH 키 미등록·호스트 키 불일치 등 접속 자체 실패.
    Auth,
    /// 원격 디렉터리·파일 쓰기 권한 없음.
    Permission,
    /// DNS·타임아웃·연결 끊김.
    Network,
    /// 원격 디스크 부족.
    DiskFull,
    /// Linux 등 미지원 OS.
    UnsupportedOs,
    /// 알 수 없는 아키텍처.
    UnsupportedArch,
    /// 로컬 바이너리를 원격 아키텍처에서 실행 불가.
    ArchMismatch,
    /// `launchctl bootstrap` 실패(원격 미로그인 등).
    Bootstrap,
    /// 프로브·원격 출력 형식 오류.
    Protocol,
    /// 그 외(원격 도구 없음·스크립트 오류 등, 상세는 `detail`).
    Other,
}

/// 설치 오류(단계·원인·원격 표준 오류 요약).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallError {
    pub kind: InstallFailureKind,
    pub step: &'static str,
    pub detail: String,
}

impl std::fmt::Display for InstallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} failed ({}): {}",
            self.step,
            kind_label(self.kind),
            self.detail
        )
    }
}

impl std::error::Error for InstallError {}

fn kind_label(kind: InstallFailureKind) -> &'static str {
    match kind {
        InstallFailureKind::Auth => "auth",
        InstallFailureKind::Permission => "permission",
        InstallFailureKind::Network => "network",
        InstallFailureKind::DiskFull => "disk-full",
        InstallFailureKind::UnsupportedOs => "unsupported-os",
        InstallFailureKind::UnsupportedArch => "unsupported-arch",
        InstallFailureKind::ArchMismatch => "arch-mismatch",
        InstallFailureKind::Bootstrap => "bootstrap",
        InstallFailureKind::Protocol => "protocol",
        InstallFailureKind::Other => "other",
    }
}

impl InstallError {
    fn new(kind: InstallFailureKind, step: &'static str, detail: impl Into<String>) -> Self {
        Self {
            kind,
            step,
            detail: detail.into(),
        }
    }
}

/// 원격 표준 오류·종료 코드로 실패 원인을 분류한다.
pub fn classify_remote_error(stderr: &str, code: Option<i32>) -> InstallFailureKind {
    if code == Some(EXIT_UNSUPPORTED_OS) || stderr.contains("krill-install: unsupported OS") {
        return InstallFailureKind::UnsupportedOs;
    }
    if code == Some(EXIT_BOOTSTRAP_FAILED)
        || (stderr.contains("krill-install: launchctl bootstrap failed")
            || stderr.contains("Bootstrap failed"))
    {
        return InstallFailureKind::Bootstrap;
    }
    let lower = stderr.to_ascii_lowercase();
    if lower.contains("no space left on device") || lower.contains("disk quota exceeded") {
        return InstallFailureKind::DiskFull;
    }
    if lower.contains("permission denied")
        || lower.contains("operation not permitted")
        || lower.contains("permissiondenied")
    {
        // SSH 접속 단계의 동일 문구는 호출자가 Auth로 올린다.
        return InstallFailureKind::Permission;
    }
    if lower.contains("host key verification failed")
        || lower.contains("permission denied (publickey")
        || lower.contains("authentication failed")
    {
        return InstallFailureKind::Auth;
    }
    if lower.contains("could not resolve hostname")
        || lower.contains("no route to host")
        || lower.contains("network is unreachable")
        || lower.contains("connection timed out")
        || lower.contains("connection reset")
        || lower.contains("connection refused")
    {
        return InstallFailureKind::Network;
    }
    if lower.contains("command not found") || lower.contains("launchctl: command not found") {
        return InstallFailureKind::Other;
    }
    InstallFailureKind::Other
}

/// 설치 결과.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallReport {
    pub version: String,
    pub bin_dir: String,
    pub bootstrapped: bool,
}

/// 전송할 바이너리 묶음(이름은 [`MANAGED_BINARIES`] 중 하나).
#[derive(Debug)]
pub struct InstallBlob<'a> {
    pub name: &'a str,
    pub bytes: &'a [u8],
}

/// 설치 전송물(바이너리 묶음 + 원격 `launchd` plist XML).
#[derive(Debug)]
pub struct InstallPayload<'a> {
    pub blobs: &'a [InstallBlob<'a>],
    pub plist_xml: &'a str,
}

/// `ssh` 한 번 실행. `stdin_bytes`가 있으면 파이프로 흘린다.
fn run_ssh(
    opts: &SshOptions,
    ssh_bin: &OsStr,
    args: &[String],
    stdin_bytes: Option<&[u8]>,
) -> io::Result<std::process::Output> {
    opts.validate()?;
    #[cfg(windows)]
    {
        ssh::windows_output(
            ssh_bin,
            args,
            stdin_bytes,
            std::time::Duration::from_secs(300),
        )
    }
    #[cfg(unix)]
    run_piped_ssh(ssh_bin, args, stdin_bytes)
}

#[cfg(unix)]
fn run_piped_ssh(
    ssh_bin: &OsStr,
    args: &[String],
    stdin_bytes: Option<&[u8]>,
) -> io::Result<std::process::Output> {
    run_piped_ssh_with_timeout(
        ssh_bin,
        args,
        stdin_bytes,
        std::time::Duration::from_secs(300),
    )
}

#[cfg(unix)]
pub(crate) fn run_piped_ssh_with_timeout(
    ssh_bin: &OsStr,
    args: &[String],
    stdin_bytes: Option<&[u8]>,
    timeout: std::time::Duration,
) -> io::Result<std::process::Output> {
    use std::io::Read as _;
    use std::os::fd::OwnedFd;
    use std::os::unix::net::UnixStream;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Instant;
    static JOBS: AtomicUsize = AtomicUsize::new(0);
    struct Permit;
    impl Drop for Permit {
        fn drop(&mut self) {
            JOBS.fetch_sub(1, Ordering::AcqRel);
        }
    }
    JOBS.fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
        (n < 4).then_some(n + 1)
    })
    .map_err(|_| io::Error::new(io::ErrorKind::WouldBlock, "managed SSH workers busy"))?;
    let _permit = Permit;
    let deadline = Instant::now() + timeout;
    // Socket pairs provide safe nonblocking stdio without FFI or detached pipe workers.
    let (mut stdout, child_out) = UnixStream::pair()?;
    let (mut stderr, child_err) = UnixStream::pair()?;
    stdout.set_nonblocking(true)?;
    stderr.set_nonblocking(true)?;
    let (mut input, child_input) = if stdin_bytes.is_some() {
        let (input, child) = UnixStream::pair()?;
        input.set_nonblocking(true)?;
        (Some(input), Stdio::from(OwnedFd::from(child)))
    } else {
        (None, Stdio::null())
    };
    let mut child = Command::new(ssh_bin)
        .args(args)
        .stdin(child_input)
        .stdout(Stdio::from(OwnedFd::from(child_out)))
        .stderr(Stdio::from(OwnedFd::from(child_err)))
        .spawn()?;
    let result = (|| {
        let mut remaining = stdin_bytes.unwrap_or_default();
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let (mut out_eof, mut err_eof) = (false, false);
        let mut status = None;
        loop {
            if Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "managed SSH timed out; remote result may be unknown",
                ));
            }
            if let Some(pipe) = input.as_mut() {
                if remaining.is_empty() {
                    input.take();
                } else {
                    match pipe.write(remaining) {
                        Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
                        Ok(n) => remaining = remaining.get(n..).unwrap_or_default(),
                        Err(error)
                            if matches!(
                                error.kind(),
                                io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                            ) => {}
                        Err(error) => return Err(error),
                    }
                }
            }
            for (pipe, bytes, eof) in [
                (&mut stdout, &mut out, &mut out_eof),
                (&mut stderr, &mut err, &mut err_eof),
            ] {
                if *eof {
                    continue;
                }
                let mut buffer = [0_u8; 8192];
                match pipe.read(&mut buffer) {
                    Ok(0) => *eof = true,
                    Ok(n) => {
                        if bytes.len().saturating_add(n) > 1024 * 1024 {
                            return Err(io::Error::new(
                                io::ErrorKind::InvalidData,
                                "managed SSH output exceeded limit; remote result may be unknown",
                            ));
                        }
                        bytes.extend_from_slice(buffer.get(..n).unwrap_or_default());
                    }
                    Err(error)
                        if matches!(
                            error.kind(),
                            io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                        ) => {}
                    Err(error) => return Err(error),
                }
            }
            if status.is_none() {
                status = child.try_wait()?;
            }
            if let Some(status) = status
                && out_eof
                && err_eof
                && input.is_none()
            {
                return Ok(std::process::Output {
                    status,
                    stdout: out,
                    stderr: err,
                });
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
    })();
    // Never enumerate or signal descendants/remote PIDs; those may outlive this management operation.
    if result.is_err() {
        child.kill().ok();
        child.wait().ok();
    }
    result
}

fn ssh_failed(opts: &SshOptions, step: &'static str, out: &std::process::Output) -> InstallError {
    let stderr = String::from_utf8_lossy(&out.stderr);
    let detail = stderr.trim();
    let detail = if detail.is_empty() {
        format!("{} exited with {}", opts.destination, out.status)
    } else if detail.chars().count() > 500 {
        detail.chars().take(500).collect()
    } else {
        detail.to_owned()
    };
    // 접속 단계 실패의 permission denied는 인증 문제로 본다.
    let mut kind = classify_remote_error(&detail, out.status.code());
    if step == "connect"
        && kind == InstallFailureKind::Permission
        && detail.to_ascii_lowercase().contains("permission denied")
    {
        kind = InstallFailureKind::Auth;
    }
    InstallError::new(kind, step, format!("{}: {detail}", opts.destination))
}

/// 디렉터리 준비용 원격 명령(bin·tmp·LaunchAgents 부모까지 만든다).
fn mkdir_remote_command(spec: &InstallSpec) -> String {
    let launch_agents = format!("{}/Library/LaunchAgents", spec.home);
    format!(
        "/bin/sh -c 'mkdir -p {} {}/tmp {}'",
        shell_quote(&spec.bin_dir()),
        shell_quote(&format!("{}/.krill/tmp", spec.home)),
        shell_quote(&launch_agents),
    )
}

/// 관리형 설치 전체 수행(디렉터리 준비 → 바이너리·plist 전송 → 설정 스크립트).
/// `local_arch`는 `std::env::consts::ARCH` 값(`aarch64`·`x86_64`)을 넘긴다.
pub fn install_remote(
    opts: &SshOptions,
    spec: &InstallSpec,
    local_arch: &str,
    blobs: &[InstallBlob<'_>],
    plist_xml: &str,
    remote: &RemoteInfo,
) -> Result<InstallReport, InstallError> {
    install_remote_with_program(
        opts,
        &ssh::ssh_program(),
        spec,
        local_arch,
        &InstallPayload { blobs, plist_xml },
        remote,
    )
}

fn install_remote_with_program(
    opts: &SshOptions,
    ssh_bin: &OsStr,
    spec: &InstallSpec,
    local_arch: &str,
    payload: &InstallPayload<'_>,
    remote: &RemoteInfo,
) -> Result<InstallReport, InstallError> {
    if !remote.platform.is_supported() {
        let kind = match remote.platform.os {
            RemoteOs::Linux | RemoteOs::Windows | RemoteOs::Other(_) => {
                InstallFailureKind::UnsupportedOs
            }
            RemoteOs::Macos => InstallFailureKind::UnsupportedArch,
        };
        // `detail`은 UI 안내문(`{os}`)에 그대로 박히므로 플랫폼 표기만 담는다.
        return Err(InstallError::new(
            kind,
            "check-platform",
            remote.platform.describe(),
        ));
    }
    // 관리형 설치는 로컬 바이너리를 그대로 보낸다. Windows PE로 원격 Mac의 실행 파일을 덮어쓰지 않는다.
    if cfg!(windows) {
        return Err(InstallError::new(
            InstallFailureKind::Other,
            "check-platform",
            "local Windows binaries cannot be installed on macOS; install Krill on the remote host first",
        ));
    }
    if !remote.platform.accepts_local_arch(local_arch) {
        return Err(InstallError::new(
            InstallFailureKind::ArchMismatch,
            "check-platform",
            remote.platform.describe(),
        ));
    }
    for blob in payload.blobs {
        if !MANAGED_BINARIES.contains(&blob.name) {
            return Err(InstallError::new(
                InstallFailureKind::Other,
                "check-blobs",
                format!("unexpected blob name: {}", blob.name),
            ));
        }
        if blob.bytes.is_empty() {
            return Err(InstallError::new(
                InstallFailureKind::Other,
                "check-blobs",
                format!("empty blob: {}", blob.name),
            ));
        }
    }
    if payload.plist_xml.trim().is_empty() {
        return Err(InstallError::new(
            InstallFailureKind::Other,
            "check-blobs",
            "empty launchd plist",
        ));
    }

    let mut base = opts.common_args(10);
    base.extend([
        "-o".to_owned(),
        "BatchMode=yes".to_owned(),
        opts.destination.clone(),
    ]);
    let run = |remote_cmd: &str, stdin: Option<&[u8]>, step: &'static str| {
        let mut args = base.clone();
        args.push(remote_cmd.to_owned());
        run_ssh(opts, ssh_bin, &args, stdin).map_err(|e| {
            InstallError::new(
                InstallFailureKind::Network,
                step,
                format!("{}: ssh spawn failed: {e}", opts.destination),
            )
        })
    };

    let out = run(&mkdir_remote_command(spec), None, "mkdir")?;
    if !out.status.success() {
        return Err(ssh_failed(opts, "mkdir", &out));
    }
    for blob in payload.blobs {
        let remote_path = format!("{}/{}", spec.bin_dir(), blob.name);
        let out = run(
            &format!("/bin/sh -c 'cat > {}'", shell_quote(&remote_path)),
            Some(blob.bytes),
            "upload",
        )?;
        if !out.status.success() {
            return Err(ssh_failed(opts, "upload", &out));
        }
    }
    let plist_out = run(
        &format!("/bin/sh -c 'cat > {}'", shell_quote(&spec.plist_path())),
        Some(payload.plist_xml.as_bytes()),
        "upload-plist",
    )?;
    if !plist_out.status.success() {
        return Err(ssh_failed(opts, "upload-plist", &plist_out));
    }
    let setup = spec.render_setup_script();
    let setup_out = run("/bin/sh -s", Some(setup.as_bytes()), "setup")?;
    if !setup_out.status.success() {
        return Err(ssh_failed(opts, "setup", &setup_out));
    }
    Ok(InstallReport {
        version: spec.version.clone(),
        bin_dir: spec.bin_dir(),
        bootstrapped: true,
    })
}

// 가짜 ssh·셸 스크립트와 unix 소켓으로 원격 설치를 흉내 내므로 unix 전용이다.
#[cfg(test)]
#[cfg(unix)]
mod tests;

#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;

    #[test]
    fn local_windows_binaries_are_rejected_before_remote_install() {
        let remote = parse_probe(
            "krill-os=Darwin\nkrill-arch=arm64\nkrill-home=/Users/test\nkrill-version=missing\n",
        )
        .unwrap();
        let spec = InstallSpec::new(&remote.home, "0.1.10").unwrap();
        let error = install_remote(
            &SshOptions::new("owner@test"),
            &spec,
            "x86_64",
            &[],
            "",
            &remote,
        )
        .unwrap_err();
        assert_eq!(error.kind, InstallFailureKind::Other);
        assert_eq!(error.step, "check-platform");
        assert!(error.detail.contains("Windows binaries"));
    }
}
