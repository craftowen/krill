//! 크래시 로그·진단 번들(MK-067, T4-28·T4-29).
//!
//! - panic hook이 `<상태 디렉터리>/crash/crash-<unix초>-<나노초>-<pid>-<프로세스>-<일련번호>.txt`(0600)에 메시지·위치·백트레이스·버전·OS를
//!   쓴다. 같은 초의 두 번째 panic도 일련번호가 달라 잃지 않는다(PH-04). 로그 체계(MK-069)와 별도 파일이다.
//!   다음 실행이 `.txt`를 보고 진단 대화상자를 띄우고, 확인하면 `.seen`으로
//!   바꾼다.
//! - 진단 번들은 크래시 로그를 모아 [`redact`]를 거친 텍스트 한 파일(`<상태 디렉터리>/diagnostics/`, 0600, 상한 4MiB)이다.
//!   터미널 출력 본문은 넣지 않는다. 업로드는 하지 않는다(opt-in 업로드는 별도 이슈).

use crate::os::fs::{DirBuilderExt as _, OpenOptionsExt as _};
use std::fmt::Write as _;
use std::fs::{self, DirBuilder, OpenOptions};
use std::io::{self, Read as _, Write as _};
use std::panic::PanicHookInfo;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use regex::Regex;

const CRASH_DIR: &str = "crash";
const BUNDLE_DIR: &str = "diagnostics";
const PENDING: &str = "txt";
const SEEN: &str = "seen";
/// 확인되지 않은 크래시 로그가 이만큼 쌓이면 더 쓰지 않는다(krilld가 세션 panic을 잡아 계속 도는 경우의 폭주 방지).
const MAX_PENDING: usize = 32;
/// 번들에 넣는 크래시 로그 하나의 상한.
const MAX_FILE: u64 = 256 * 1024;
/// 번들 전체 상한(Orca 진단 번들과 같은 4MiB).
const MAX_BUNDLE: usize = 4 * 1024 * 1024;
const MASK: &str = "[REDACTED]";

/// `<상태 디렉터리>/crash`.
#[must_use]
pub fn crash_dir(state: &Path) -> PathBuf {
    state.join(CRASH_DIR)
}

/// 기존 hook(기본 stderr 출력)을 유지한 채 크래시 로그를 먼저 쓴다. 상태 디렉터리를 모르거나 쓰기에 실패해도
/// panic 처리는 그대로 이어진다.
pub fn install_panic_hook(process: &'static str) {
    let Some(dir) = crate::daemon::state_dir().map(|s| crash_dir(&s)) else {
        return;
    };
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        write_crash(&dir, process, info).ok();
        prev(info);
    }));
}

/// 같은 초에 여러 panic이 와도 파일명이 겹치지 않게 하는 프로세스 내 일련번호(PH-04).
/// krilld는 세션 작업 단위 `catch_unwind`로 panic 뒤에도 계속 돌 수 있어, 같은 버그가 여러
/// 세션에서 동시에 터지면 같은 초에 여러 보고가 온다.
static CRASH_SEQ: AtomicU64 = AtomicU64::new(0);

fn write_crash(dir: &Path, process: &str, info: &PanicHookInfo<'_>) -> io::Result<()> {
    if list(dir, PENDING).len() >= MAX_PENDING {
        return Ok(());
    }
    DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
    let now = unix_secs();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos());
    let seq = CRASH_SEQ.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    let thread = std::thread::current();
    let mut text = header("minikrill crash report", now);
    // `String`에 쓰는 `write!`는 실패하지 않는다.
    writeln!(text, "process: {process}").ok();
    writeln!(text, "pid: {pid}").ok();
    writeln!(text, "thread: {}", thread.name().unwrap_or("<unnamed>")).ok();
    writeln!(
        text,
        "message: {}",
        info.payload_as_str().unwrap_or("<non-string payload>")
    )
    .ok();
    if let Some(loc) = info.location() {
        writeln!(text, "location: {loc}").ok();
    }
    writeln!(
        text,
        "\nbacktrace:\n{}",
        std::backtrace::Backtrace::force_capture()
    )
    .ok();
    let path = dir.join(crash_filename(now, nanos, pid, process, seq));
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?
        .write_all(text.as_bytes())
}

fn header(title: &str, now: u64) -> String {
    format!(
        "{title}\nversion: {}\nos: {} {}\ntime: {now} (unix)\n",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH,
    )
}

fn unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// 크래시 보고 파일명. `create_new(true)`로 열므로 같은 프로세스·같은 초의 두 번째 panic도
/// 일련번호(`seq`)가 달라 겹치지 않는다(PH-04). `crash-` 접두·`txt` 확장은 [`pending`]·[`list`] 약속이다.
fn crash_filename(now: u64, nanos: u32, pid: u32, process: &str, seq: u64) -> String {
    format!("crash-{now}-{nanos:09}-{pid}-{process}-{seq}.{PENDING}")
}

/// `dir`에서 `crash-*.<ext>`를 이름순(= 시각순)으로.
fn list(dir: &Path, ext: &str) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<_> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.extension().is_some_and(|e| e == ext)
                && p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with("crash-"))
        })
        .collect();
    out.sort();
    out
}

/// 아직 보고(대화상자 확인)하지 않은 크래시 로그.
#[must_use]
pub fn pending(state: &Path) -> Vec<PathBuf> {
    list(&crash_dir(state), PENDING)
}

/// 세션 기록 실패 경고 파일(상태 디렉터리 바로 아래). krilld가 쓰고 `krill doctor`가 읽는다(OWE-605).
pub const RECORD_WARNINGS_FILE: &str = "record-warnings.log";

/// 기록 실패 경고 줄. 없거나 비어 있으면 빈 벡터다.
#[must_use]
pub fn record_warnings(state: &Path) -> Vec<String> {
    fs::read_to_string(state.join(RECORD_WARNINGS_FILE))
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect()
}

/// 확인한 크래시 로그를 `.seen`으로 바꿔 다음 실행에 다시 묻지 않는다. 번들에는 계속 들어간다.
// ponytail: `.seen`은 지우지 않는다. 쌓이는 양은 확인 횟수 × MAX_PENDING이 상한, 문제가 되면 오래된 것부터 정리.
pub fn acknowledge(logs: &[PathBuf]) {
    for p in logs {
        fs::rename(p, p.with_extension(SEEN)).ok();
    }
}

/// 리댁션한 진단 번들을 `<상태 디렉터리>/diagnostics/minikrill-diag-<unix초>.txt`(0600)로 쓰고 경로를 돌려준다.
/// 크래시 로그는 최신 것부터 넣고 4MiB를 넘기면 멈춘다.
pub fn write_bundle(state: &Path) -> io::Result<PathBuf> {
    let now = unix_secs();
    let mut text = header("minikrill diagnostic bundle", now);
    let dir = crash_dir(state);
    let mut logs = list(&dir, PENDING);
    logs.extend(list(&dir, SEEN));
    logs.sort();
    for log in logs.iter().rev() {
        let mut raw = Vec::new();
        fs::File::open(log)?.take(MAX_FILE).read_to_end(&mut raw)?;
        let name = log
            .file_name()
            .map(|n| n.to_string_lossy())
            .unwrap_or_default();
        let section = format!("\n==== {name} ====\n{}\n", String::from_utf8_lossy(&raw));
        if text.len().saturating_add(section.len()) > MAX_BUNDLE {
            text.push_str("\n==== (older crash logs omitted: size limit) ====\n");
            break;
        }
        text.push_str(&section);
    }
    let out = state.join(BUNDLE_DIR);
    DirBuilder::new().recursive(true).mode(0o700).create(&out)?;
    let path = out.join(format!("minikrill-diag-{now}.txt"));
    OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&path)?
        .write_all(redact(&text).as_bytes())?;
    Ok(path)
}

#[expect(
    clippy::expect_used,
    reason = "불변식: 상수 패턴이라 컴파일이 실패하지 않는다(테스트가 확인)"
)]
fn rule(pattern: &str) -> Regex {
    Regex::new(pattern).expect("redaction pattern")
}

/// 비밀로 보이는 것을 `[REDACTED]`로 가린다: PEM 블록(잘린 것 포함), `sk-ant-`·`sk-`·GitHub·Slack·AWS 토큰,
/// `Authorization:`·`Bearer` 값, 이름에 token·secret·password·key·auth가 들어간 `KEY=VALUE`/`"key": "value"`의 값,
/// `/Users/<이름>`의 사용자명.
#[must_use]
pub fn redact(text: &str) -> String {
    static RULES: LazyLock<[(Regex, &str); 5]> = LazyLock::new(|| {
        [
            (
                rule(r"(?s)-----BEGIN [A-Z0-9 ]+-----.*?(?:-----END [A-Z0-9 ]+-----|\z)"),
                "[REDACTED PEM]",
            ),
            (
                rule(
                    r"\b(?:sk-ant-[A-Za-z0-9_\-]*|sk-[A-Za-z0-9_\-]{16,}|gh[pousr]_[A-Za-z0-9]{16,}|github_pat_[A-Za-z0-9_]{16,}|xox[abprs]-[A-Za-z0-9\-]{10,}|AKIA[0-9A-Z]{16})",
                ),
                MASK,
            ),
            (
                rule(r"(?i)(authorization:\s*(?:bearer\s+|basic\s+)?|bearer\s+)[^\s\x22',]+"),
                "${1}[REDACTED]",
            ),
            (
                rule(
                    r#"(?i)\b([a-z0-9_\-]*(?:token|secret|passw(?:or)?d|api_?key|access_?key|private_?key|credential|auth)[a-z0-9_\-]*)("?\s*[:=]\s*"?)[^\s"',;&]+"#,
                ),
                "${1}${2}[REDACTED]",
            ),
            (rule(r"/Users/[^/\s\x22':]+"), "/Users/[USER]"),
        ]
    });
    let mut out = text.to_owned();
    for (re, with) in RULES.iter() {
        if let std::borrow::Cow::Owned(s) = re.replace_all(&out, *with) {
            out = s;
        }
    }
    out
}

#[cfg(test)]
mod tests;
