//! 앱·데몬 실행 표식. 오류 보고에 동의한 동안 인스턴스마다 하나를 두고, 다음 유지보수가 정상 종료 표시 없이
//! 끝난 실행을 `abnormal_exit`(`unclean_exit`)로 회수한다. 원인(SIGKILL·OOM 등)은 단정하지 않는다.
//!
//! - `<상태>/error-reporting/runs/<run_id>.run`(0600)은 고정 길이 ASCII 레코드다: 상태·panic 표시·프로세스·pid·시작
//!   시각·동의 세대 해시·발견 시각·release·build. 살아 있는 동안 주인이 파일 잠금(`File::try_lock`, macOS·Linux는
//!   `flock`)을 쥔다. std가 연 fd는 `O_CLOEXEC`라 자식 프로세스가 잠금을 물려받지 않는다.
//! - 정상 반환(Drop)과 의도적 종료([`RunGuard::finish`])는 상태 바이트만 clean으로 제자리에 쓴다. 주인은 파일을
//!   지우지 않는다: 그 전에 파일을 연 스캐너가 고아 inode의 잠금을 얻어 끝난 실행으로 오판하기 때문이다. panic 중
//!   Drop·SIGKILL·`exec`는 running으로 남는다.
//! - 지우는 쪽은 스캐너뿐이다. 잠금을 얻은 뒤 경로가 아직 같은 dev/ino인지 다시 보고 지운다. 잠금을 못 얻으면
//!   살아 있는 실행으로 보고 pid 생존 여부는 묻지 않는다.
//! - 같은 실행에 panic 증거([`note_panic`])나 native 보고서가 있으면 abnormal을 내지 않는다. OS 보고서가 늦게 쓰일 수
//!   있어 처음 발견한 뒤 [`GRACE_MS`]가 지나야 기록한다. 현재 동의 세대의 표식만 회수하고 나머지는 조용히 지운다.
//! - CLI·holder에는 표식이 없다. Windows는 아직 표식을 만들지 않는다(`None`).

use std::io;
use std::path::Path;

use super::Process;

#[cfg(unix)]
use std::fs::{self, DirBuilder, File, Metadata, OpenOptions};
#[cfg(unix)]
use std::io::Write as _;
#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt as _, FileExt as _, MetadataExt as _, OpenOptionsExt as _};
#[cfg(unix)]
use std::path::PathBuf;
#[cfg(unix)]
use std::sync::{Arc, Mutex, PoisonError, TryLockError};

#[cfg(unix)]
use super::types::{is_hex32, random_id};
#[cfg(unix)]
use super::{Code, Consent, Event};

/// 처음 발견한 끝난 실행을 abnormal로 기록하기 전에 기다리는 시간(밀리초, 10분). 늦게 쓰이는 OS 보고서가 이긴다.
pub const GRACE_MS: u64 = 600_000;
/// 발견 뒤 이만큼 지나도 기록하지 못한 표식은 기록 없이 지운다(밀리초, 7일 — 대기열 보존 기간과 같다).
#[cfg(unix)]
const GIVE_UP_MS: u64 = 604_800_000;
/// 시각 비교 여유(밀리초). OS 보고서 시각은 초 단위로 자른다.
#[cfg(unix)]
const CLOCK_SLACK_MS: u64 = 2_000;
/// 프로세스 시작에서 표식 생성까지 허용하는 간격(밀리초, 10분).
#[cfg(unix)]
const LAUNCH_WINDOW_MS: u64 = 600_000;
/// 표식 디렉터리 항목이 이만큼 있으면 새 표식을 만들지 않는다.
#[cfg(unix)]
const MAX_MARKERS: usize = 32;
/// 스캔 한 번에 보는 디렉터리 항목 상한.
#[cfg(unix)]
const MAX_SCAN: usize = 256;
/// 스캔 한 번에 잠그는 끝난 실행 상한.
#[cfg(unix)]
const MAX_RECLAIM: usize = 32;
/// panic 표시를 남길 이 프로세스의 표식 상한.
#[cfg(unix)]
const MAX_ACTIVE: usize = 8;

#[cfg(unix)]
const MARKER_EXT: &str = ".run";
#[cfg(unix)]
const GENERATION_DOMAIN: &str = "krill.error-reporting.run-generation.v1";
#[cfg(unix)]
const ABNORMAL_DOMAIN: &str = "krill.error-reporting.abnormal.v1";

/// 레코드 형식. 필드는 공백 하나로 나누고 release·build는 `#`로 오른쪽을 채운다.
/// `KRUN1 <R|C> <-|P> <A|D> <pid 10> <시작 ms 20> <run_id 32> <세대 해시 32> <발견 ms 20> <release 64> <build 80>\n`
#[cfg(unix)]
const MAGIC: &str = "KRUN1";
#[cfg(unix)]
const RECORD_LEN: usize = 277;
#[cfg(unix)]
const STATE_AT: u64 = 6;
#[cfg(unix)]
const PANIC_AT: u64 = 8;
#[cfg(unix)]
const SEEN_AT: u64 = 110;
#[cfg(unix)]
const RELEASE_WIDTH: usize = 64;
#[cfg(unix)]
const BUILD_WIDTH: usize = 80;
#[cfg(unix)]
const CLEAN: u8 = b'C';
#[cfg(unix)]
const PANICKED: u8 = b'P';

/// 이 프로세스의 살아 있는 표식. panic hook이 잠금 없이 기다리지 않고 표시를 남길 수 있게 fd를 공유한다.
#[cfg(unix)]
static ACTIVE: Mutex<Vec<Arc<File>>> = Mutex::new(Vec::new());

/// 실행 중인 앱·데몬 인스턴스의 표식. 살아 있는 동안 파일 잠금을 쥔다.
#[derive(Debug)]
pub struct RunGuard {
    #[cfg(unix)]
    marker: Arc<File>,
    _private: (),
}

impl RunGuard {
    /// 현재 동의(환경 거부 포함)에 속하는 앱·데몬 실행만 표식을 만든다. CLI·holder·동의 없음·표식 상한·Windows는
    /// `None`이다. 실패해도 앱·데몬 동작은 그대로 이어져야 한다.
    ///
    /// # Errors
    /// 상태 디렉터리·표식 파일 쓰기 실패.
    #[cfg_attr(
        not(unix),
        expect(
            clippy::unnecessary_wraps,
            reason = "Windows has no run markers yet; callers share one signature"
        )
    )]
    pub fn start(state: &Path, process: Process) -> io::Result<Option<Self>> {
        #[cfg(unix)]
        {
            if !matches!(process, Process::App | Process::Daemon) {
                return Ok(None);
            }
            let Some(consent) = super::consent_at(state)? else {
                return Ok(None);
            };
            Self::start_with(state, process, &consent)
        }
        #[cfg(not(unix))]
        {
            let _ = (state, process);
            Ok(None)
        }
    }

    /// [`Self::start`]와 같지만 이미 읽은 동의를 쓴다. 표식을 쓴 뒤 동의 파일이 `consent`와 다르면 clean으로 남기고
    /// `None`이다(철회·재동의 경계를 넘는 표식을 남기지 않는다).
    #[cfg(unix)]
    pub(super) fn start_with(
        state: &Path,
        process: Process,
        consent: &Consent,
    ) -> io::Result<Option<Self>> {
        Self::start_attempt(state, process, consent, true)
    }

    #[cfg(unix)]
    fn start_attempt(
        state: &Path,
        process: Process,
        consent: &Consent,
        retry: bool,
    ) -> io::Result<Option<Self>> {
        if !matches!(process, Process::App | Process::Daemon) {
            return Ok(None);
        }
        let runs = super::dir(state).join(super::RUNS_DIR);
        // 동의 파일이 있는 오류 보고 디렉터리 아래에만 만든다(재귀 생성 없음).
        match DirBuilder::new().mode(0o700).create(&runs) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        }
        if !fs::symlink_metadata(&runs)?.file_type().is_dir() || at_capacity(&runs) {
            return Ok(None);
        }
        let build = Event::new(process, Code::UncleanExit, None)?;
        let record = Record {
            clean: false,
            panicked: false,
            process,
            pid: std::process::id(),
            started_at_ms: super::now_ms(),
            run_id: random_id()?,
            generation: generation_hash(consent),
            seen_ms: 0,
            release: build.release,
            build_id: build.build_id,
        };
        let Some(bytes) = record.encode() else {
            return Ok(None);
        };
        let path = runs.join(format!("{}{MARKER_EXT}", record.run_id));
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)?;
        if file.try_lock().is_err() {
            return Ok(None);
        }
        // 쓰다 실패하면 손상 레코드로 남고, 잠금이 풀린 뒤 스캐너가 기록 없이 지운다.
        file.write_all(&bytes)?;
        // A scanner can acquire and delete an empty file before our first lock.
        // Never retain an orphaned inode; retry once with a new instance ID.
        if !marker_is_current(&file, &path) {
            return if retry {
                Self::start_attempt(state, process, consent, false)
            } else {
                Ok(None)
            };
        }
        if super::consent::read(state)?.as_ref() != Some(consent) {
            file.write_all_at(&[CLEAN], STATE_AT).ok();
            return Ok(None);
        }
        let marker = Arc::new(file);
        register(&marker);
        Ok(Some(Self {
            marker,
            _private: (),
        }))
    }

    /// `exec` 핸드오프·명시적 종료처럼 Drop이 돌지 않는 의도적 종료 직전에 clean을 쓴다.
    pub fn finish(self) {
        #[cfg(unix)]
        self.marker.write_all_at(&[CLEAN], STATE_AT).ok();
        drop(self);
    }
}

impl Drop for RunGuard {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            unregister(&self.marker);
            // panic으로 풀리는 중이면 종료가 정상인지 모르므로 running으로 남긴다.
            if !std::thread::panicking() {
                self.marker.write_all_at(&[CLEAN], STATE_AT).ok();
            }
        }
    }
}

/// panic hook에서 부른다. 이 프로세스의 살아 있는 표식에 panic 표시를 제자리에 쓴다. 표식 목록을 다른 스레드가
/// 쥐고 있으면 기다리지 않고 건너뛴다.
pub fn note_panic() {
    #[cfg(unix)]
    {
        let active = match ACTIVE.try_lock() {
            Ok(active) => active,
            Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
            Err(TryLockError::WouldBlock) => return,
        };
        for marker in active.iter() {
            marker.write_all_at(&[PANICKED], PANIC_AT).ok();
        }
    }
}

#[cfg(unix)]
fn register(marker: &Arc<File>) {
    let mut active = ACTIVE.lock().unwrap_or_else(PoisonError::into_inner);
    if active.len() < MAX_ACTIVE {
        active.push(Arc::clone(marker));
    }
}

#[cfg(unix)]
fn unregister(marker: &Arc<File>) {
    ACTIVE
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .retain(|m| !Arc::ptr_eq(m, marker));
}

/// 표식 디렉터리가 꽉 찼거나 읽을 수 없으면 참(새 표식을 만들지 않는다).
#[cfg(unix)]
fn at_capacity(runs: &Path) -> bool {
    fs::read_dir(runs).map_or(true, |entries| {
        entries.take(MAX_MARKERS).count() >= MAX_MARKERS
    })
}

#[cfg(unix)]
fn is_marker_name(name: &str) -> bool {
    name.strip_suffix(MARKER_EXT).is_some_and(is_hex32)
}

/// 원문 세대 대신 도메인 분리 해시를 표식에 둔다.
#[cfg(unix)]
fn generation_hash(consent: &Consent) -> String {
    super::event_id(GENERATION_DOMAIN, consent.generation.as_bytes())
}

/// 표식 레코드.
#[cfg(unix)]
#[derive(Debug, Clone, PartialEq, Eq)]
struct Record {
    clean: bool,
    panicked: bool,
    process: Process,
    pid: u32,
    started_at_ms: u64,
    run_id: String,
    generation: String,
    seen_ms: u64,
    release: String,
    build_id: String,
}

#[cfg(unix)]
impl Record {
    /// 정확히 [`RECORD_LEN`]바이트. 필드가 형식을 벗어나면 `None`.
    fn encode(&self) -> Option<Vec<u8>> {
        let process = match self.process {
            Process::App => 'A',
            Process::Daemon => 'D',
            Process::Cli | Process::Holder => return None,
        };
        if !is_hex32(&self.run_id) || !is_hex32(&self.generation) {
            return None;
        }
        let text = format!(
            "{MAGIC} {} {} {process} {:010} {:020} {} {} {:020} {} {}\n",
            if self.clean { 'C' } else { 'R' },
            if self.panicked { 'P' } else { '-' },
            self.pid,
            self.started_at_ms,
            self.run_id,
            self.generation,
            self.seen_ms,
            pad(&self.release, RELEASE_WIDTH)?,
            pad(&self.build_id, BUILD_WIDTH)?,
        );
        (text.len() == RECORD_LEN).then(|| text.into_bytes())
    }

    /// 형식이 조금이라도 다르면 `None`(손상으로 보고 기록 없이 지운다).
    fn decode(bytes: &[u8]) -> Option<Self> {
        if bytes.len() != RECORD_LEN {
            return None;
        }
        let line = std::str::from_utf8(bytes).ok()?.strip_suffix('\n')?;
        let mut fields = line.split(' ');
        if fields.next()? != MAGIC {
            return None;
        }
        let clean = match fields.next()? {
            "R" => false,
            "C" => true,
            _ => return None,
        };
        let panicked = match fields.next()? {
            "-" => false,
            "P" => true,
            _ => return None,
        };
        let process = match fields.next()? {
            "A" => Process::App,
            "D" => Process::Daemon,
            _ => return None,
        };
        let pid = digits(fields.next()?, 10)?.parse().ok()?;
        let started_at_ms = digits(fields.next()?, 20)?.parse().ok()?;
        let run_id = fields.next().filter(|s| is_hex32(s))?.to_owned();
        let generation = fields.next().filter(|s| is_hex32(s))?.to_owned();
        let seen_ms = digits(fields.next()?, 20)?.parse().ok()?;
        let release = unpad(fields.next()?, RELEASE_WIDTH)?;
        let build_id = unpad(fields.next()?, BUILD_WIDTH)?;
        if fields.next().is_some() {
            return None;
        }
        Some(Self {
            clean,
            panicked,
            process,
            pid,
            started_at_ms,
            run_id,
            generation,
            seen_ms,
            release,
            build_id,
        })
    }
}

/// release·build 문자 집합(`[A-Za-z0-9.+-]`, 비어 있지 않음). 서버 검증은 [`Event::validate`]가 다시 한다.
#[cfg(unix)]
fn plain(s: &str, width: usize) -> bool {
    !s.is_empty()
        && s.len() <= width
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'+' | b'-'))
}

#[cfg(unix)]
fn pad(s: &str, width: usize) -> Option<String> {
    if !plain(s, width) {
        return None;
    }
    let mut out = String::with_capacity(width);
    out.push_str(s);
    out.push_str(&"#".repeat(width.checked_sub(s.len())?));
    Some(out)
}

#[cfg(unix)]
fn unpad(s: &str, width: usize) -> Option<String> {
    if s.len() != width {
        return None;
    }
    let value = s.trim_end_matches('#');
    plain(value, width).then(|| value.to_owned())
}

#[cfg(unix)]
fn digits(s: &str, width: usize) -> Option<&str> {
    (s.len() == width && s.bytes().all(|b| b.is_ascii_digit())).then_some(s)
}

/// 스캐너가 잠근, 정상 종료 표시가 없는 현재 세대 실행. 잠금은 이 값을 놓을 때 풀린다.
#[cfg(unix)]
#[derive(Debug)]
pub(super) struct Stale {
    file: File,
    path: PathBuf,
    record: Record,
    native: bool,
}

#[cfg(unix)]
impl Stale {
    /// 표식 디렉터리를 상한 안에서 훑는다. 잠금을 얻은 clean·손상·다른 세대 표식은 기록 없이 지우고, 현재 세대의
    /// 끝난 실행만 잠근 채 돌려준다. 잠기지 않는(살아 있는) 표식은 건드리지 않는다.
    pub(super) fn scan(state: &Path, consent: &Consent) -> Vec<Self> {
        let runs = super::dir(state).join(super::RUNS_DIR);
        let Ok(entries) = fs::read_dir(&runs) else {
            return Vec::new();
        };
        let generation = generation_hash(consent);
        let mut out = Vec::new();
        for entry in entries.take(MAX_SCAN) {
            if out.len() >= MAX_RECLAIM {
                break;
            }
            let Ok(entry) = entry else { continue };
            let name = entry.file_name();
            if !name.to_str().is_some_and(is_marker_name) {
                continue;
            }
            let path = runs.join(name);
            if let Some(stale) = claim(path, &generation) {
                out.push(stale);
            }
        }
        out
    }

    /// 같은 실행의 OS 보고서인가: 프로세스·pid가 같고, 보고 시각이 표식 이후이며, 알면 시작 시각이 표식 직전이다.
    pub(super) fn matches(
        &self,
        process: Process,
        pid: u32,
        capture_ms: u64,
        launch_ms: Option<u64>,
    ) -> bool {
        let r = &self.record;
        r.process == process
            && r.pid == pid
            && capture_ms.saturating_add(CLOCK_SLACK_MS) >= r.started_at_ms
            && launch_ms.is_none_or(|launch| {
                launch <= r.started_at_ms.saturating_add(CLOCK_SLACK_MS)
                    && r.started_at_ms.saturating_sub(launch) <= LAUNCH_WINDOW_MS
            })
    }

    /// 이 실행의 native 보고서를 기록했다(abnormal을 내지 않는다).
    pub(super) fn claim_native(&mut self) {
        self.native = true;
    }

    /// 표식을 쓴 빌드의 release·build.
    pub(super) fn build(&self) -> (&str, &str) {
        (&self.record.release, &self.record.build_id)
    }
}

/// 잠금을 얻고 경로가 같은 파일인지 다시 확인한다. 정리만 하고 끝난 표식은 `None`.
#[cfg(unix)]
fn claim(path: PathBuf, generation: &str) -> Option<Stale> {
    use rustix::fs::{Mode, OFlags};
    let fd = rustix::fs::open(
        &path,
        OFlags::RDWR | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC | OFlags::NOCTTY,
        Mode::empty(),
    )
    .ok()?;
    claim_open(File::from(fd), path, generation)
}

/// 연 표식의 잠금을 얻은 뒤 경로가 아직 그 inode인지 확인한다. 열고 나서 다른 스캐너가 지운 고아 inode는 잠금을
/// 얻어도 끝난 실행이 아니다.
#[cfg(unix)]
fn claim_open(file: File, path: PathBuf, generation: &str) -> Option<Stale> {
    if !file.metadata().ok()?.file_type().is_file() {
        return None;
    }
    // 실패(WouldBlock 포함)는 살아 있는 실행이다.
    file.try_lock().ok()?;
    // 길이는 잠근 뒤에 읽는다. 다른 스캐너가 이미 지웠거나 바뀐 경로면 이 inode는 더 이상 표식이 아니다.
    let held = file.metadata().ok()?;
    if !same_file(&held, &fs::symlink_metadata(&path).ok()?) {
        return None;
    }
    let record = read_record(&file, &held);
    match record {
        Some(record) if !record.clean && record.generation == generation => Some(Stale {
            file,
            path,
            record,
            native: false,
        }),
        _ => {
            fs::remove_file(&path).ok();
            None
        }
    }
}

#[cfg(unix)]
fn same_file(a: &Metadata, b: &Metadata) -> bool {
    a.file_type().is_file() && b.file_type().is_file() && a.dev() == b.dev() && a.ino() == b.ino()
}

#[cfg(unix)]
fn marker_is_current(file: &File, path: &Path) -> bool {
    match (file.metadata(), fs::symlink_metadata(path)) {
        (Ok(held), Ok(current)) => same_file(&held, &current),
        _ => false,
    }
}

#[cfg(unix)]
fn read_record(file: &File, meta: &Metadata) -> Option<Record> {
    if usize::try_from(meta.len()).ok()? != RECORD_LEN {
        return None;
    }
    let mut bytes = [0u8; RECORD_LEN];
    file.read_exact_at(&mut bytes, 0).ok()?;
    Record::decode(&bytes)
}

/// 잠근 끝난 실행을 정리한다. native·panic 증거가 있으면 기록 없이 지우고, 처음 본 실행은 발견 시각만 쓰고
/// 기다리며, [`GRACE_MS`]가 지난 실행은 결정적 ID의 `unclean_exit`를 기록한 뒤 지운다. 기록한 수를 돌려준다.
#[cfg(unix)]
pub(super) fn settle(state: &Path, consent: &Consent, now_ms: u64, stale: Vec<Stale>) -> usize {
    let mut recorded = 0usize;
    for run in stale {
        let record = &run.record;
        if run.native || record.panicked {
            fs::remove_file(&run.path).ok();
            continue;
        }
        // 처음 보았거나 시계가 크게 뒤로 갔으면 지금부터 기다린다.
        if record.seen_ms == 0 || record.seen_ms > now_ms.saturating_add(GRACE_MS) {
            run.file
                .write_all_at(format!("{now_ms:020}").as_bytes(), SEEN_AT)
                .ok();
            continue;
        }
        let waited = now_ms.saturating_sub(record.seen_ms);
        if waited < GRACE_MS {
            continue;
        }
        let queued =
            abnormal_event(record).map(|event| super::record_with_consent(state, event, consent));
        match queued {
            Some(Ok(true)) => {
                recorded = recorded.saturating_add(1);
                fs::remove_file(&run.path).ok();
            }
            // 대기열이 차거나 바쁘면 다음 유지보수에서 같은 ID로 다시 시도한다(중복은 ID로 걸러진다).
            Some(Ok(false) | Err(_)) if waited < GIVE_UP_MS => {}
            _ => {
                fs::remove_file(&run.path).ok();
            }
        }
    }
    recorded
}

/// run ID에서 결정적으로 얻은 ID와 표식을 쓴 빌드의 release·build로 만든 `unclean_exit`.
#[cfg(unix)]
fn abnormal_event(record: &Record) -> Option<Event> {
    let mut event = Event::new(record.process, Code::UncleanExit, None).ok()?;
    event.event_id = super::event_id(ABNORMAL_DOMAIN, record.run_id.as_bytes());
    event.release.clone_from(&record.release);
    event.build_id.clone_from(&record.build_id);
    event.validate().ok()?;
    Some(event)
}

#[cfg(test)]
#[path = "runs_tests.rs"]
pub(super) mod tests;
