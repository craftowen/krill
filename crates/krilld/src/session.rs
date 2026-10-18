//! 세션 하나 = PTY 리더 스레드 하나 + 엔진(Term·파서).
//!
//! - 리더는 절대 잠들어 모으지 않는다. 직전 read가 PTY 한 번 분량(1,024B)으로 꽉 찼을 때만 최대
//!   [`SPIN`] 동안 다시 읽어 묶는다(ARCHITECTURE §4.2, perf-first #1).
//! - DEC 2026 sync는 vte가 알려 주는 마감 시각을 `poll` 타임아웃으로 한 번만 건다. 폴링 없음(perf-first #7).
//! - 엔진 호출은 세션 단위 `catch_unwind`로 감싼다(CONVENTIONS P4). panic한 묶음은 Gap으로 기록하고 엔진을
//!   새로 만든 뒤 계속 읽는다. 다른 세션 스레드와 PTY는 영향이 없다.
//! - 질의 응답은 krilld만 한다(AGENTS 3, MK-020). 엔진이 만든 응답을 리더가 엔진 잠금 안에서 바로 PTY에 쓴다.
//!   앱이 붙어 있지 않아도 답한다.
//! - 읽은 원본은 엔진 결과와 상관없이 세그먼트 저장소에 넘긴다([`Recorder`], MK-024). EOF에서 봉인한다.
//! - 화면 전송(MK-023): 엔진이 바뀔 때마다 화면 revision을 올리고, 원본은 디스크에 넘긴 뒤 메모리 링([`RING`])에
//!   넣고 구독자를 깨운다([`Tap`]). 깨우기만 하고 기다리지 않는다. 디스크에 먼저 쓰므로 링에서 밀려난 원본은
//!   (기록했다면) 디스크에 있다.
//! - krilld 간 인계([`crate::rejoin`]): [`halt`]가 이 프로세스의 리더를 모두 읽기 경계에서 멈추고 세그먼트를 봉인한다.
//!   멈춘 리더는 세션을 닫힌 것으로 알리지 않는다(세션은 다음 krilld가 이어 읽는다).
//! - 숨긴 세션 강등(MK-035): 화면 구독자([`Viewer`])가 없는 채로 [`PARK_GRACE`]가 지나면 엔진의 핫 스크롤백·파서
//!   버퍼를 돌려준다(화면·모드는 유지). 구독자가 생기면 바로 다시 쌓는다. 버린 행은 세그먼트 콜드 페이지로 읽으므로
//!   디스크에 기록하지 않는 세션은 강등하지 않는다.

mod input;

use std::collections::VecDeque;
use std::io;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::panic::{self, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use krill_proto::SessionId;
use krill_proto::holder::WinSize;
use krill_proto::screen::{Cell, Revision, Screen, Seq};
use rustix::event::{PollFd, PollFlags, Timespec, poll};
use rustix::fs::{OFlags, fcntl_getfl, fcntl_setfl};
use rustix::io::{Errno, read, write};

use crate::appearance::Appearance;
use crate::store::{Piece, Recorder, Store};
use crate::term::{Engine, Feed};

/// macOS PTY master의 read 한 번 최대 크기(perf-first 실측).
const PTY_CHUNK: usize = 1_024;
/// 엔진에 한 번에 넣는 최대 바이트.
const BATCH: usize = 64 * 1_024;
/// 꽉 찬 read 뒤 다시 읽는 시간(perf-first 실측 범위 5~20µs의 가운데).
// ponytail: 고정 상수. tokio AsyncFd 위 효과는 미측정(perf-first 미확인) — 런타임을 바꾸면 다시 잰다.
const SPIN: Duration = Duration::from_micros(10);
/// 세션마다 메모리에 두는 최근 원본(원본 스트림 구독자의 큐). 넘게 밀린 구독자는 디스크에서 읽는다.
// ponytail: 고정 256KiB 비압축(세션 10개 2.5MB). 메모리가 빠듯하면 zstd 링(ARCHITECTURE §3) 또는 크기 축소.
pub(crate) const RING: usize = 256 * 1_024;
/// 화면 구독자가 모두 떠난 뒤 강등까지 기다리는 시간. 탭을 오가는 동안 강등·복원이 반복되지 않게 한다.
pub const PARK_GRACE: Duration = Duration::from_secs(10);

/// [`halt`] 이후 리더는 읽지 않고 끝난다.
static HALTED: AtomicBool = AtomicBool::new(false);
/// 리더가 master와 함께 poll하는 파이프(읽는 쪽, 쓰는 쪽). [`halt`]가 한 바이트를 쓰고 아무도 읽지 않아 모든 리더의
/// poll이 깨어 있는 채로 남는다. 프로세스에 하나.
static HALT: OnceLock<(OwnedFd, OwnedFd)> = OnceLock::new();

fn halt_pipe() -> io::Result<&'static (OwnedFd, OwnedFd)> {
    if let Some(p) = HALT.get() {
        return Ok(p);
    }
    let (r, w) = io::pipe()?;
    Ok(HALT.get_or_init(|| (r.into(), w.into())))
}

/// 이 프로세스의 PTY 리더를 모두 멈춘다(krilld 간 인계). 리더는 읽은 묶음을 기록까지 마친 뒤 세그먼트를 봉인하고
/// 끝난다. 되돌릴 수 없다. 끝났는지는 [`Session::is_closed`]로 본다.
pub(crate) fn halt() -> io::Result<()> {
    let (_, w) = halt_pipe()?;
    HALTED.store(true, Ordering::SeqCst);
    write(w, &[0])?;
    Ok(())
}

/// 엔진이 panic해서 화면에 반영하지 못한 원본 바이트 구간(`start`는 세션 시작부터 읽은 바이트 수).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Gap {
    pub start: u64,
    pub len: u64,
}

/// 엔진과 그 화면의 revision·반영한 원본 끝. 같은 잠금 안에서만 바뀌어 캡처가 일관된 셋을 얻는다.
struct Live {
    engine: Engine,
    revision: u64,
    seq: u64,
    /// 원본 수신 시각. 화면 변화·sync flush·resize와 무관하게 원본 바이트만 갱신한다.
    last_output: Instant,
    /// 완료 뒤 미전송 프롬프트도 보호하는 입력 시각.
    input_since_done: bool,
    last_input_ns: u64,
    done_ns: u64,
    /// 화면 구독자 수([`Viewer`]).
    viewers: usize,
    /// 구독자가 0이 될 때마다 올린다. 강등 타이머는 자기가 본 값과 같을 때만 강등한다.
    epoch: u64,
    /// 강등된 상태([`Engine::demote`]).
    parked: bool,
    /// 엔진에 적용했지만 아직 기록기에 넘기지 않은 창 크기. 리더가 다음 원본 앞에 기록한다(엔진과 같은 순서).
    resized: Option<WinSize>,
    /// 엔진의 현재 크기. panic 복구 때 엔진을 다시 만드는 데 쓴다. 시작 크기가 아니라
    /// 마지막 리사이즈 크기여야 화면 격자가 PTY 크기와 어긋나지 않는다(K-19).
    size: WinSize,
}

/// 구독자가 기다리는 것: 원본 링과 변경 알림.
struct Out {
    /// 최근 원본. 맨 앞 바이트의 seq는 `end - ring.len()`.
    ring: VecDeque<u8>,
    end: u64,
    /// 마지막으로 알린 화면 revision.
    revision: u64,
    closed: bool,
}

/// [`Tap::wait`]이 돌려주는 세션 상태.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pulse {
    pub revision: u64,
    pub end: u64,
    pub closed: bool,
}

struct Shared {
    id: SessionId,
    clients: Mutex<crate::attach::roles::Clients>,
    /// 사용자 입력 프레임 하나를 끝까지 쓰는 동안 쥔다(프레임끼리 섞이지 않게). `clients`와 따로 둬 입력이 막혀도
    /// 새 연결·역할·크기 요청은 막히지 않는다(OWE-579).
    input: Mutex<input::State>,
    /// PTY 쓰기(질의 응답·알림)는 모두 `live` 잠금 안에서 해서 서로 섞이지 않는다.
    live: Mutex<Live>,
    gaps: Mutex<Vec<Gap>>,
    master: OwnedFd,
    look: Arc<Appearance>,
    out: Mutex<Out>,
    changed: Condvar,
    /// 링 밖 원본을 읽을 디스크 저장소(기록하지 않는 세션은 `None`).
    store: Option<Arc<Store>>,
}

impl Shared {
    /// 엔진 변경을 알린다. `bytes`는 방금 디스크에 넘긴 원본.
    fn publish(&self, bytes: &[u8], revision: u64, closed: bool) {
        let mut out = lock(&self.out);
        let held = out.ring.len();
        let over = (held + bytes.len()).saturating_sub(RING);
        out.ring.drain(..over.min(held));
        out.ring.extend(
            bytes
                .get(bytes.len().saturating_sub(RING)..)
                .unwrap_or_default(),
        );
        out.end += bytes.len() as u64;
        out.revision = out.revision.max(revision);
        out.closed |= closed;
        drop(out);
        self.changed.notify_all();
    }

    /// 구독자가 0이 된 지 [`PARK_GRACE`]가 지났다. 그 사이 구독자가 오갔으면(`epoch`가 다르면) 아무것도 안 한다.
    fn park(&self, epoch: u64) {
        let mut live = lock(&self.live);
        if live.viewers > 0 || live.epoch != epoch || live.parked {
            return;
        }
        let look = &*self.look;
        live.parked = panic::catch_unwind(AssertUnwindSafe(|| live.engine.demote(look))).is_ok();
    }
}

/// `epoch`의 구독자 0 상태가 [`PARK_GRACE`] 동안 이어지면 강등한다. 디스크 기록이 없는 세션은 강등하지 않는다.
fn park_later(shared: &Arc<Shared>, epoch: u64) {
    if shared.store.is_none() {
        return;
    }
    let weak = Arc::downgrade(shared);
    // ponytail: 구독자가 0이 될 때마다 [`PARK_GRACE`] 자는 스레드 하나(세션 fd를 잡지 않게 Weak). 탭을 빠르게 오가면
    // 그만큼 쌓였다 끝난다. 수백 개가 쌓이는 사용이 보이면 krilld 전역 타이머 스레드 하나로 모은다.
    thread::Builder::new()
        .name("park".to_owned())
        .spawn(move || {
            thread::sleep(PARK_GRACE);
            if let Some(shared) = weak.upgrade() {
                shared.park(epoch);
            }
        })
        .ok();
}

/// 화면 구독자 하나(MK-035, [`Tap::watch`]). 들고 있는 동안 세션은 핫 스크롤백을 쌓는다.
pub(crate) struct Viewer(Arc<Shared>);

impl Drop for Viewer {
    fn drop(&mut self) {
        let mut live = lock(&self.0.live);
        live.viewers = live.viewers.saturating_sub(1);
        if live.viewers == 0 {
            live.epoch = live.epoch.wrapping_add(1);
            let epoch = live.epoch;
            drop(live);
            park_later(&self.0, epoch);
        }
    }
}

/// krilld가 붙든 세션 하나. master fd는 리더 스레드와 함께 들고, 둘 다 놓으면 닫힌다.
pub struct Session {
    id: SessionId,
    shared: Arc<Shared>,
    reader: JoinHandle<io::Result<()>>,
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

impl Session {
    /// master fd를 non-blocking으로 바꾸고 리더 스레드를 띄운다. `rec`이 `None`이면 디스크에 기록하지 않는다.
    pub fn start(
        id: SessionId,
        master: OwnedFd,
        size: WinSize,
        look: Arc<Appearance>,
        rec: Option<Recorder>,
    ) -> io::Result<Self> {
        fcntl_setfl(&master, fcntl_getfl(&master)? | OFlags::NONBLOCK)?;
        let halt = halt_pipe()?.0.as_fd();
        // krilld가 재기동했으면 seq는 디스크 마지막 세그먼트 끝에서 이어진다.
        let seq = rec.as_ref().map_or(0, Recorder::seq);
        let shared = Arc::new(Shared {
            id,
            clients: Mutex::default(),
            input: Mutex::default(),
            live: Mutex::new(Live {
                engine: Engine::new(size),
                revision: 0,
                seq,
                last_output: Instant::now(),
                input_since_done: true,
                last_input_ns: 0,
                done_ns: 0,
                viewers: 0,
                epoch: 0,
                parked: false,
                resized: None,
                size,
            }),
            gaps: Mutex::new(Vec::new()),
            master,
            look,
            out: Mutex::new(Out {
                ring: VecDeque::new(),
                end: seq,
                revision: 0,
                closed: false,
            }),
            changed: Condvar::new(),
            store: rec
                .as_ref()
                .filter(|r| r.store().recording())
                .map(|r| Arc::clone(r.store())),
        });
        let reader = thread::Builder::new()
            .name(format!("pty-{}", id.0))
            .spawn({
                let shared = Arc::clone(&shared);
                move || {
                    if let Some(r) = &rec {
                        rebuild(&shared, r, size);
                    }
                    let r = read_loop(&shared, halt, rec);
                    if !HALTED.load(Ordering::SeqCst)
                        && panic::catch_unwind(AssertUnwindSafe(|| {
                            shared.publish(&[], 0, true);
                        }))
                        .is_err()
                    {
                        tracing::warn!("pty close publish panicked");
                    }
                    r
                }
            })?;
        // 아무도 붙지 않은 채 시작한 세션(CLI 생성·krilld 재기동)도 같은 규칙으로 강등한다.
        park_later(&shared, 0);
        Ok(Self { id, shared, reader })
    }

    pub fn id(&self) -> SessionId {
        self.id
    }

    /// 보이는 화면 평문(진단·테스트용).
    pub fn screen_text(&self) -> String {
        lock(&self.shared.live).engine.screen_text()
    }

    /// 화면·원본 구독 핸들(MK-023). 세션이 끝나도 쓸 수 있다(마지막 화면·남은 원본).
    pub fn tap(&self) -> Tap {
        Tap(Arc::clone(&self.shared))
    }

    /// [`Appearance::set_theme`]이 `true`를 돌려준 뒤 부른다. 모드 2031을 켠 세션이면 `CSI ?997;Nn`을 쓴다.
    pub fn theme_changed(&self) {
        let theme = self.shared.look.theme();
        let live = lock(&self.shared.live);
        if let Some(notice) = live.engine.scheme_notice(&theme) {
            write_reply(&self.shared.master, notice.as_bytes());
        }
    }

    /// master fd(krilld 간 인계에서 다음 krilld에 넘긴다).
    pub(crate) fn master(&self) -> BorrowedFd<'_> {
        self.shared.master.as_fd()
    }

    pub fn gaps(&self) -> Vec<Gap> {
        lock(&self.shared.gaps).clone()
    }

    /// PTY가 닫혀(EOF) 리더가 끝났는지.
    pub fn is_closed(&self) -> bool {
        self.reader.is_finished()
    }

    /// 리더가 끝날 때까지 기다리고 끝난 이유를 돌려준다.
    pub fn wait(self) -> io::Result<()> {
        self.reader
            .join()
            .unwrap_or_else(|_| Err(io::Error::other("pty reader panicked outside catch_unwind")))
    }
}

/// 세션 화면·원본 구독 핸들(MK-023, [`crate::attach`]). 읽기만 하고 PTY 리더를 멈추지 않는다.
#[derive(Clone)]
pub struct Tap(Arc<Shared>);

impl std::fmt::Debug for Tap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("Tap").field(&self.0.id).finish()
    }
}

/// [`Tap::read`]의 결과.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Read {
    /// `pos`부터의 원본(링에서).
    Bytes(Vec<u8>),
    /// `pos`가 링보다 앞이다. `[pos, to)`는 [`Tap::disk`]로.
    Disk { to: u64 },
    /// 새 원본이 없다.
    Idle(Pulse),
}

/// 묶음 종료 동안 새 attach·입력·출력을 막는다. holder fd는 계속 유지한다.
pub(crate) struct HibernationGuard<'a> {
    _input: MutexGuard<'a, input::State>,
    clients: MutexGuard<'a, crate::attach::roles::Clients>,
    _live: MutexGuard<'a, Live>,
}

impl HibernationGuard<'_> {
    pub(crate) fn sleeping(&mut self) {
        self.clients.sleeping = true;
    }
}

impl Tap {
    /// 원본 출력 정지 시간(MK-047). 재기동 시 보수적으로 0부터 센다.
    pub(crate) fn quiet_ms(&self) -> u64 {
        u64::try_from(lock(&self.0.live).last_output.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    /// 새로 수락한 완료 hook만 입력 보호 기준을 갱신한다(벽시계 역행에도 입력은 보호된다).
    pub(crate) fn agent_done(&self, ns: u64) {
        let mut live = lock(&self.0.live);
        live.done_ns = ns;
        live.input_since_done = live.last_input_ns >= ns;
    }

    /// 연결·입력·출력과 직렬화한 최종 검사. 묶음 전체의 guard를 모은 뒤 종료를 시작한다.
    pub(crate) fn hibernation_guard(
        &self,
        done_ns: u64,
        idle: Duration,
    ) -> Option<HibernationGuard<'_>> {
        let input = self.0.input.try_lock().ok()?;
        let clients = lock(&self.0.clients);
        let live = lock(&self.0.live);
        if clients.sleeping
            || !clients.is_empty()
            || live.viewers != 0
            || live.done_ns != done_ns
            || live.input_since_done
            || live.last_output.elapsed() < idle
            || clients.last_attached.is_some_and(|t| t.elapsed() < idle)
        {
            return None;
        }
        Some(HibernationGuard {
            _input: input,
            clients,
            _live: live,
        })
    }

    /// 화면 구독을 시작한다(MK-035). 강등된 세션이면 핫 스크롤백을 다시 쌓기 시작한다. 화면을 뜨기 전에 부른다.
    pub(crate) fn watch(&self) -> Viewer {
        let mut live = lock(&self.0.live);
        live.viewers = live.viewers.saturating_add(1);
        if live.parked {
            let look = &*self.0.look;
            // panic하면 강등 상태로 남는다(스크롤백만 덜 쌓인다).
            live.parked =
                panic::catch_unwind(AssertUnwindSafe(|| live.engine.promote(look))).is_err();
        }
        Viewer(Arc::clone(&self.0))
    }

    /// 핫 스크롤백을 쥐고 있지 않은(강등된) 세션인지.
    pub fn is_parked(&self) -> bool {
        lock(&self.0.live).parked
    }

    pub(crate) fn clients(&self) -> &Mutex<crate::attach::roles::Clients> {
        &self.0.clients
    }

    pub(crate) fn size(&self) -> io::Result<WinSize> {
        let _live = lock(&self.0.live);
        let size = rustix::termios::tcgetwinsize(&self.0.master)?;
        Ok(WinSize {
            cols: size.ws_col,
            rows: size.ws_row,
        })
    }

    pub fn id(&self) -> SessionId {
        self.0.id
    }

    /// 보이는 화면과 그 revision·원본 끝을 한 잠금 안에서 뜬다.
    pub fn capture(&self) -> (Screen, Revision, Seq) {
        let (screen, revision, seq, _) = self.capture_with_modes();
        (screen, revision, seq)
    }

    /// OSC 52 쓰기 수와, 그 수가 `seen`보다 늘었으면 마지막 값(MK-033). 엔진을 새로 만들면 수가 0으로 돌아간다.
    pub(crate) fn clipboard(&self, seen: u64) -> (u64, Option<String>) {
        let live = lock(&self.0.live);
        let (n, text) = live.engine.clipboard();
        (n, (n > seen).then(|| text.to_owned()))
    }

    /// 화면·입력 모드를 같은 출력 revision에서 캡처한다(MK-029).
    pub(crate) fn capture_with_modes(
        &self,
    ) -> (Screen, Revision, Seq, krill_proto::term_modes::Modes) {
        let live = lock(&self.0.live);
        (
            live.engine.screen(),
            Revision(live.revision),
            Seq(live.seq),
            live.engine.input_modes(),
        )
    }

    /// 보조 상태 감지용 제목·평문·revision. `seen`과 revision이 같으면 문자열화 없이 `None`이다.
    /// 제목·화면이 바뀌면 revision도 오르므로(출력·리사이즈) 캐시 무효화에 revision 하나로 충분하다.
    pub(crate) fn evidence_since(&self, seen: Option<u64>) -> Option<(String, String, u64)> {
        let live = lock(&self.0.live);
        if seen == Some(live.revision) {
            return None;
        }
        Some((
            live.engine.title().to_owned(),
            live.engine.screen_text(),
            live.revision,
        ))
    }

    /// 전경 프로세스 그룹. PTY 소유권은 holder에 그대로 둔다.
    pub(crate) fn foreground(&self) -> Option<u32> {
        crate::sys::tcgetpgrp(self.0.master.as_fd())
    }

    /// CLI 화면과 원본 위치를 한 잠금 안에서 뜬다.
    pub fn capture_cli(
        &self,
        format: krill_proto::control::CaptureFormat,
    ) -> io::Result<(Seq, Vec<u8>)> {
        let live = lock(&self.0.live);
        let bytes = live.engine.capture_cli(format)?;
        let rewind = u64::try_from(live.engine.capture_rewind()).unwrap_or(u64::MAX);
        let seq = Seq(live.seq.saturating_sub(rewind));
        drop(live);
        // guarded() advances live.seq before recorder.append()/publish(). Do not hand a raw
        // subscriber a future position while that same-daemon publish is still in flight.
        self.wait(|p| p.end >= seq.0 || p.closed);
        Ok((seq, bytes))
    }

    /// PTY와 에뮬레이터를 함께 바꾸고 화면 구독자를 깨운다.
    /// 거대 크기는 와이어 상한으로 자른다(K-01). PTY만 키우면 엔진과 어긋나
    /// 프로그램 줄바꿈이 화면과 달라지므로 둘 다 같은 값으로 둔다.
    pub fn resize(&self, size: WinSize) -> io::Result<()> {
        let size = crate::term::clamp_size(size);
        let mut live = lock(&self.0.live);
        rustix::termios::tcsetwinsize(
            &self.0.master,
            rustix::termios::Winsize {
                ws_col: size.cols,
                ws_row: size.rows,
                ws_xpixel: 0,
                ws_ypixel: 0,
            },
        )?;
        live.engine.resize(size);
        live.size = size;
        live.resized = Some(size);
        live.revision = live.revision.wrapping_add(1);
        let revision = live.revision;
        drop(live);
        self.0.publish(&[], revision, false);
        Ok(())
    }

    /// 핫 스크롤백 행 수와, 아래에서 `skip`행 위부터 위로 최대 `count`행(오래된 행부터).
    pub fn history(&self, skip: usize, count: usize) -> (usize, Vec<Vec<Cell>>) {
        lock(&self.0.live).engine.history_rows(skip, count)
    }

    /// 디스크 저장소(콜드 페이지·링 밖 원본). 기록하지 않는 세션이면 `None`.
    pub fn store(&self) -> Option<&Arc<Store>> {
        self.0.store.as_ref()
    }

    /// 지금 상태.
    pub fn pulse(&self) -> Pulse {
        let out = lock(&self.0.out);
        Pulse {
            revision: out.revision,
            end: out.end,
            closed: out.closed,
        }
    }

    /// `ready`가 참이 될 때까지 기다린다. 다른 스레드가 `ready`가 보는 값을 바꾸면 [`Tap::wake`]를 불러야 한다.
    pub fn wait(&self, ready: impl Fn(&Pulse) -> bool) -> Pulse {
        let mut out = lock(&self.0.out);
        loop {
            let p = Pulse {
                revision: out.revision,
                end: out.end,
                closed: out.closed,
            };
            if ready(&p) {
                return p;
            }
            out = self
                .0
                .changed
                .wait(out)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }

    /// [`Tap::wait`] 중인 스레드를 모두 깨운다(조건을 다시 본다).
    pub fn wake(&self) {
        // 기다리는 쪽이 조건을 본 뒤 잠들기 전에 알림이 지나가지 않도록 잠금을 한 번 거친다.
        drop(lock(&self.0.out));
        self.0.changed.notify_all();
    }

    /// 원본 `pos`부터 최대 `max`바이트를 링에서 꺼낸다.
    pub fn read(&self, pos: u64, max: usize) -> Read {
        let out = lock(&self.0.out);
        let start = out.end - out.ring.len() as u64;
        if pos < start {
            return Read::Disk { to: start };
        }
        if pos >= out.end {
            return Read::Idle(Pulse {
                revision: out.revision,
                end: out.end,
                closed: out.closed,
            });
        }
        let skip = usize::try_from(pos - start).unwrap_or(usize::MAX);
        Read::Bytes(out.ring.iter().skip(skip).take(max).copied().collect())
    }

    /// 사용자 입력을 PTY에 다 쓴다(MK-026). 다른 입력과는 프레임 단위로, 질의 응답과는 `write` 한 번 단위로(엔진 잠금)
    /// 섞이지 않는다. 입력 큐가 차면(에이전트가 stdin을 안 읽음, EAGAIN) 엔진 잠금을 놓고 쓸 수 있을 때까지 기다린다.
    /// 리더는 막지 않는다. `peer`는 입력을 보낸 연결이다: 잠금·쓰기 대기 중 상대가 닫으면 남은 본문을 버린다.
    /// 열린 bracketed paste는 종료 표지만 보충한다(프레임 사이의 끊김도 연결 정리 시 처리).
    ///
    /// # Errors
    /// 세션이 끝났거나, `peer`가 닫혔거나, PTY 쓰기가 실패했다(입력 일부만 썼을 수 있다).
    // ponytail: 자식이 stdin을 영영 안 읽으면 붙여넣기 종료 표지를 보충하는 스레드는 세션이 닫힐 때까지 기다린다.
    // 엔진·연결 장부는 잠그지 않으며 다른 입력 대기자는 HUP으로 취소할 수 있다. 세션 쓰기 큐가 생기면 그리로 옮긴다.
    pub fn write_input(&self, bytes: &[u8], peer: Option<BorrowedFd<'_>>) -> io::Result<()> {
        input::write_input(self, bytes, peer)
    }

    /// 입력 연결이 프레임 사이에서 닫혀도 열린 붙여넣기의 종료 표지를 보충한다.
    pub(crate) fn finish_input(&self) {
        input::finish_input(self);
    }

    /// 원본 `[from, to)`를 디스크에서 꺼낸다. 기록하지 않는 세션이면 통째로 Gap.
    pub fn disk(
        &self,
        from: u64,
        to: u64,
        each: impl FnMut(Piece) -> io::Result<()>,
    ) -> io::Result<()> {
        if let Some(store) = &self.0.store {
            return store.raw(self.0.id, from, to, each);
        }
        let mut each = each;
        each(Piece::Gap {
            seq: from,
            len: to.saturating_sub(from),
        })
    }
}

/// 엔진 panic은 `catch_unwind`가 잠금 안에서 잡으므로 독(poison)은 생기지 않는다. 생겨도 값은 쓸 수 있다.
pub(crate) fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// 엔진 호출 하나를 격리하고, 나온 질의 응답을 PTY에 쓴다. panic하면 엔진을 현재 크기로
/// 새로 만들고 `false`. 어느 쪽이든 화면이 바뀌었을 수 있으니 revision을 올리고 원본 끝을
/// `fed`만큼 민다. 새 revision과, 원본을 넣었으면 그 앞에 엔진에 적용된 아직 기록하지 않은
/// 창 크기를 돌려준다(같은 잠금 안에서 꺼내 기록 순서 = 엔진 순서).
fn guarded(
    shared: &Shared,
    fed: usize,
    f: impl FnOnce(&mut Engine) -> String,
) -> (bool, u64, Option<WinSize>) {
    let mut live = lock(&shared.live);
    let mut resized = None;
    if fed > 0 {
        live.last_output = Instant::now();
        resized = live.resized.take();
    }
    let ok = if let Ok(reply) = panic::catch_unwind(AssertUnwindSafe(|| f(&mut live.engine))) {
        write_reply(&shared.master, reply.as_bytes());
        true
    } else {
        // ponytail: 화면을 빈 Term으로 되돌린다. 데몬 권위 키프레임(MK-006 포크 승인 대기)이 생기면 마지막
        // 키프레임 + 원본 재생으로 복원하고, 같은 바이트로 재차 panic할 때만 그 구간을 건너뛴다(ARCHITECTURE §4).
        // 시작 크기가 아니라 현재 크기로 만든다. 리사이즈 뒤 panic에 예전 격자로 돌아가면
        // 화면이 PTY 크기와 다음 resize까지 어긋난다(K-19).
        let size = live.size;
        live.engine = Engine::new(size);
        if live.parked {
            let look = &*shared.look;
            live.parked =
                panic::catch_unwind(AssertUnwindSafe(|| live.engine.demote(look))).is_ok();
        }
        false
    };
    live.revision += 1;
    live.seq += fed as u64;
    (ok, live.revision, resized)
}

/// krilld 재기동(업그레이드·크래시) 뒤 디스크 마지막 세그먼트로 화면과 핫 스크롤백을 되살린다(ARCHITECTURE §4.2,
/// MK-062). 기록 끝이 세션 seq와 같을 때만 쓴다. 읽기는 그 뒤에 시작하므로 그동안의 출력은 커널 PTY 버퍼에서 기다린다.
/// 남아 있는 연속 세그먼트를 시작점까지 재생하므로 비용은 그 원본 길이에 비례한다(K-18).
/// 실패·panic이면 빈 화면으로 시작한다.
// ponytail: 앞 원본이 GC된 경우 남은 첫 세그먼트의 ANSI 키프레임으로 근사 복원한다.
// 엔진 전체 상태 키프레임이 생기면 앞 세그먼트 재생 없이 복원할 수 있다.
fn rebuild(shared: &Shared, rec: &Recorder, size: WinSize) {
    let end = rec.seq();
    if end == 0 {
        return; // 새 세션
    }
    let restored = panic::catch_unwind(AssertUnwindSafe(|| {
        let r = rec
            .store()
            .history(shared.id)
            .and_then(|h| h.restore())
            .ok()?;
        (r.end_seq == end).then(|| {
            let mut engine = r.into_engine();
            engine.resize(size);
            engine
        })
    }));
    let Ok(Some(engine)) = restored else { return };
    let rev = {
        let mut live = lock(&shared.live);
        live.engine = engine;
        live.size = size;
        live.revision += 1;
        live.revision
    };
    shared.publish(&[], rev, false);
}

fn read_loop(shared: &Shared, halt: BorrowedFd<'_>, mut rec: Option<Recorder>) -> io::Result<()> {
    let master = &shared.master;
    let look = &*shared.look;
    let mut buf = vec![0u8; BATCH];
    let mut seq = lock(&shared.live).seq;
    let mut errors = 0u32;
    loop {
        if HALTED.load(Ordering::SeqCst) {
            if let Some(r) = &mut rec {
                seal(shared, r);
            }
            return Ok(());
        }
        let deadline = lock(&shared.live).engine.sync_deadline();
        let timeout = match deadline {
            Some(at) => {
                let left = at.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    let (_, rev, _) = guarded(shared, 0, |e| e.end_sync(Feed::Live, look));
                    shared.publish(&[], rev, false);
                    continue;
                }
                // 변환에 실패하는 시각은 사실상 없지만, 실패해도 리더를 끝내지 않고 타임아웃 없이 기다린다.
                Timespec::try_from(left).ok()
            }
            None => None,
        };
        let mut fds = [
            PollFd::new(master, PollFlags::IN),
            PollFd::new(&halt, PollFlags::IN),
        ];
        match poll(&mut fds, timeout.as_ref()) {
            // 마감 도달·halt는 다음 바퀴 맨 위에서 반영
            Ok(0) | Err(Errno::INTR) => continue,
            Ok(_) if HALTED.load(Ordering::SeqCst) => continue,
            Ok(_) => {}
            // poll 오류로 리더를 끝내면 PTY를 읽는 쪽이 없어 세션 출력이 영구 정지한다.
            // 백오프 후 다시 기다린다(K-05).
            Err(e) => {
                errors = errors.saturating_add(1);
                tracing::warn!(error = ?e, consecutive = errors, "pty poll failed; retrying");
                thread::sleep(backoff(errors));
                continue;
            }
        }
        let Some(n) = (match read_batch(master, &mut buf) {
            Ok(n) => n,
            Err(e) => {
                errors = errors.saturating_add(1);
                tracing::warn!(kind = ?e.kind(), consecutive = errors, "pty read failed; retrying");
                thread::sleep(backoff(errors));
                continue;
            }
        }) else {
            if let Some(r) = &mut rec {
                seal(shared, r);
            }
            return Ok(()); // EOF
        };
        errors = 0;
        let Some(bytes) = buf.get(..n) else { continue };
        let capture = rec.as_ref().is_some_and(Recorder::needs_keyframe);
        let mut snapshot = None;
        let (ok, rev, resized) = guarded(shared, n, |e| {
            // resize와 같은 잠금 안에서 이번 원본을 feed하기 전 상태를 캡처한다.
            // 미완성 제어열·sync는 이전 세그먼트 워밍업에 맡긴다.
            if capture {
                if let Ok(Ok(bytes)) = panic::catch_unwind(AssertUnwindSafe(|| e.cold_keyframe())) {
                    snapshot = bytes;
                } else {
                    tracing::warn!("cold keyframe capture failed; continuing session");
                }
            }
            e.feed(bytes, Feed::Live, look)
        });
        // 엔진 호출은 `guarded`가 잡지만 기록·발행 panic은 여기서 잡는다. 이 묶음은
        // Gap으로 남기고 다음 묶음부터 계속 읽는다. 리더가 죽으면 PTY를 읽는 쪽이
        // 없어 세션 출력이 영구 정지한다(K-05).
        let recorded = publish_batch(shared, seq, bytes, rev, || {
            if !ok {
                lock(&shared.gaps).push(Gap {
                    start: seq,
                    len: n as u64,
                });
            }
            if let Some(r) = &mut rec {
                if let Some(size) = resized {
                    r.resize(size);
                }
                if let Some(snapshot) = snapshot {
                    r.keyframe(snapshot);
                }
                r.append(bytes);
            }
        });
        if !recorded {
            rec = rec.take().and_then(|recorder| {
                // unwind 뒤의 인코더를 다시 쓰지 않는다. 디스크의 온전한 꼬리부터
                // 새 기록기를 열고 빠진 seq를 Gap으로 넘긴다. 복구 실패도 PTY를 멈추지 않는다.
                panic::catch_unwind(AssertUnwindSafe(|| {
                    let size = lock(&shared.live).size;
                    recorder.recover_after_panic(seq.saturating_add(n as u64), size)
                }))
                .ok()
                .and_then(|result| {
                    if let Err(error) = &result {
                        tracing::warn!(kind = ?error.kind(), "recorder panic recovery failed");
                    }
                    result.ok()
                })
            });
        }
        seq += n as u64;
    }
}

/// 기록 panic이 화면·원본 발행에 미치는 영향을 한 경계에서 검사한다.
fn publish_batch(shared: &Shared, seq: u64, bytes: &[u8], rev: u64, record: impl FnOnce()) -> bool {
    let recorded = panic::catch_unwind(AssertUnwindSafe(record)).is_ok();
    if !recorded {
        tracing::warn!("pty batch recording panicked; kept as gap");
        lock(&shared.gaps).push(Gap {
            start: seq,
            len: bytes.len() as u64,
        });
    }
    // 디스크 실패와 원본 링 발행은 독립이다. 발행을 건너뛰면 out.end가 live.seq보다
    // 영구히 뒤처져 capture/search가 기다리거나 원본 바이트 위치가 어긋난다.
    shared.publish(bytes, rev, false);
    recorded
}

/// PTY 읽기 오류 재시도 간격. 25ms부터 두 배로 최대 800ms(K-05). 영구 오류에도
/// 리더를 끝내지 않고(세션 출력 정지 방지) CPU를 쓰지 않는다.
fn backoff(consecutive: u32) -> Duration {
    Duration::from_millis(25 << consecutive.min(5))
}

/// 끝나기 전 남은 창 크기를 기록하고 봉인한다(콜드 복원이 마지막 크기로 그린다).
/// 봉인 중 panic해도 리더를 끝내지 않는다. 리더가 죽으면 PTY를 읽는 쪽이 없어진다(K-05).
fn seal(shared: &Shared, rec: &mut Recorder) {
    if panic::catch_unwind(AssertUnwindSafe(|| {
        if let Some(size) = lock(&shared.live).resized.take() {
            rec.resize(size);
        }
        rec.seal();
    }))
    .is_err()
    {
        tracing::warn!("pty seal panicked; continuing shutdown");
    }
}

/// 읽을 수 있게 된 master에서 한 묶음을 읽는다. `None`이면 EOF(슬레이브가 모두 닫힘).
fn read_batch(master: &OwnedFd, buf: &mut [u8]) -> io::Result<Option<usize>> {
    let mut len = 0;
    let mut spin_until: Option<Instant> = None;
    while let Some(rest) = buf.get_mut(len..).filter(|r| !r.is_empty()) {
        match read(master, rest) {
            Ok(0) | Err(Errno::IO) if len == 0 => return Ok(None),
            Ok(0) | Err(Errno::IO) => break, // 남은 것부터 넣고 다음 바퀴에서 EOF
            Ok(n) => {
                len += n;
                if n < PTY_CHUNK {
                    break;
                }
                spin_until = Some(Instant::now() + SPIN);
            }
            Err(Errno::INTR) => {}
            Err(Errno::AGAIN) => match spin_until {
                Some(until) if Instant::now() < until => std::hint::spin_loop(),
                _ => break,
            },
            // 읽은 게 있으면 그 묶음부터 처리하고 오류는 다음 바퀴에 다시 본다.
            // 바로 끝내면 PTY를 읽는 쪽이 없어 세션 출력이 영구 정지한다(K-05).
            Err(_) if len > 0 => break,
            Err(e) => return Err(e.into()),
        }
    }
    Ok(Some(len))
}

/// 질의 응답을 PTY 입력으로 쓴다. 리더를 멈추지 않으려고 기다리지 않는다.
fn write_reply(master: &OwnedFd, mut bytes: &[u8]) {
    while !bytes.is_empty() {
        match write(master, bytes) {
            Ok(n) if n > 0 => bytes = bytes.get(n..).unwrap_or_default(),
            Err(Errno::INTR) => {}
            // ponytail: 입력 큐가 가득하면(에이전트가 stdin을 안 읽음, EAGAIN) 남은 응답을 버린다. 응답은 질의당 수십
            // 바이트라 드물다. UI 입력 쓰기(MK-026)가 생기면 같은 세션 쓰기 큐로 보내 순서를 지키며 기다린다.
            Ok(_) | Err(_) => return,
        }
    }
}

#[cfg(test)]
#[path = "session/tests.rs"]
mod tests;
