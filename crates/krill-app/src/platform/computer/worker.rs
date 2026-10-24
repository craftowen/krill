//! 워커 실행 정책. 리더 스레드가 접수·상태·취소를 맡고, 호출 스레드(실행기)가 요청을 하나씩 실행한다.
//!
//! - 상태·취소는 리더 스레드가 장부만 보고 답하므로 실행기가 AX 호출에 막혀도 응답한다.
//! - 변경 동작은 전역 입력 잠금 → 대상 재검증 → `begin_dispatch` → 발행 순서다. 잠금 대기·재검증 중
//!   취소·기한 초과는 미실행이다. 발행 뒤 결과 불명(`unknown`)이면 실행기는
//!   잠금을 쥔 채 정지(quiescing)하고 이후 변경 요청을 받지 않는다. 잠금은 프로세스 종료로만 풀린다.
//!   정리 미완료(`cleanup: incomplete`)도 같다.
//! - `batch`는 잠금 하나를 모든 단계 동안 쥐고, 단계마다 기한·멈춤 요청 확인 → 재검증 → 발행 순서다. 첫 단계만
//!   `begin_dispatch`를 거치며, 발행 뒤 취소는 남은 단계를 막는다. 한 단계라도 제출되지 않았거나 검증이
//!   충족되지 않았거나 정리가 끝나지 않았으면 멈춘다. 발행한 단계는 다시 시도하지 않는다.
//! - 같은 seq는 결과와 무관하게 한 번만 실행된다(`ledger`).
//! - 출력은 [`Outbox`]가 전담한다. stdout 쓰기는 쓰기 스레드만 하고, 리더·실행기는 상한 있는 대기열에 넣기만 한다.
//!   호출자가 읽지 않아 한 프레임 쓰기가 정체되거나 호출자가 사라지면 전송로를 실패로 확정하고, 그 뒤 새 요청을
//!   실행하지 않은 채 종료 코드 3으로 끝난다.

use std::collections::VecDeque;
use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::sync::mpsc::{self, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use super::ledger::{Admission, Ledger};
use super::lock::{self, InputLock};
use super::protocol::{
    CONTRACT, Cleanup, Delivery, ErrorInfo, Inbound, LEDGER_CAPACITY, Limits, MAX_DEADLINE_MS,
    MAX_FRAME_BYTES, Observation, Op, Outbound, Outcome, Parsed, QUEUE_CAPACITY, RejectCode,
    RequestFrame, RequestState, Verification, diagnostic, encode, parse_line, read_frame,
};

/// `batch`가 멈춘 뒤 발행했던 것이 있으면 붙는 오류 코드.
pub const BATCH_STOPPED: &str = "batch_stopped";

/// 종료 코드: 출력 전송로 실패(호출자가 읽지 않음·사라짐·쓰기 정체). 미전달 프레임이 있을 수 있다.
pub const EXIT_TRANSPORT: u8 = 3;
/// 아직 쓰지 못한 출력 프레임 수·바이트 상한. 차면 넣는 쪽이 자리가 날 때까지 기다린다(리더가 기다리면 stdin
/// 읽기도 멈춰 호출자에게 역압이 걸린다).
const OUT_QUEUE_FRAMES: usize = 64;
const OUT_QUEUE_BYTES: usize = 4 * MAX_FRAME_BYTES;
/// 접수 답(`accepted`·`rejected`·`duplicate`)을 넣기 전에 확보하는 자리. 진단 문구 상한 덕에 이보다 작다.
const REPLY_ROOM: usize = 4096;
/// 실행기가 쉬는 동안 전송로 상태를 다시 보는 간격.
const IDLE_POLL: Duration = Duration::from_millis(100);
/// 정상 종료 때 남은 출력을 내보내며 기다리는 상한.
const FLUSH_TIMEOUT: Duration = Duration::from_secs(5);

/// 이 실행기 인스턴스의 신원. 재시작마다 새 값이며 이전 참조는 거절된다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    pub epoch: String,
    pub context: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    pub code: &'static str,
    pub message: String,
}

impl Refusal {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

#[derive(Debug)]
pub struct ReadResult {
    pub observation: Observation,
    pub data: serde_json::Value,
}

#[derive(Debug)]
pub struct Dispatched {
    pub delivery: Delivery,
    pub verification: Verification,
    pub cleanup: Cleanup,
    pub data: Option<serde_json::Value>,
}

/// 플랫폼 실행기. `prepare`는 부작용 없이 대상을 재검증하고, `dispatch`만 입력을 낸다.
pub trait Backend {
    type Prepared;
    fn supported(&self) -> bool;
    fn capabilities(&self) -> Vec<&'static str>;
    fn unsupported(&self) -> Vec<&'static str>;
    fn read(&mut self, op: &Op, deadline: Instant) -> Result<ReadResult, Refusal>;
    fn prepare(&mut self, op: &Op, deadline: Instant) -> Result<Self::Prepared, Refusal>;
    fn dispatch(&mut self, prepared: Self::Prepared, deadline: Instant) -> Dispatched;
}

#[derive(Debug, Clone)]
pub struct Config {
    pub lock_path: PathBuf,
    /// 한 프레임 쓰기가 이보다 오래 막히면 전송로 실패다.
    pub write_stall: Duration,
}

/// 프로덕션 쓰기 정체 상한. 1 MiB 프레임 하나를 읽는 데 이보다 오래 걸리는 호출자는 읽지 않는 것으로 본다.
pub const WRITE_STALL: Duration = Duration::from_secs(10);

struct Job {
    frame: RequestFrame,
    deadline: Instant,
}

struct Shared {
    ledger: Mutex<Ledger>,
    outbox: Outbox,
}

fn guard<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// 출력 전송로. stdout은 전용 쓰기 스레드 하나만 쓰고, 다른 스레드는 직렬화한 프레임을 상한 있는 대기열
/// ([`OUT_QUEUE_FRAMES`]·[`OUT_QUEUE_BYTES`])에 넣는다. 대기열이 차면 넣는 쪽이 기다린다 — 느린 호출자는 역압을
/// 받을 뿐 프레임을 잃지 않는다. 어떤 스레드도 IO를 하며 이 잠금을 쥐지 않는다.
///
/// 한 프레임 쓰기가 `write_stall`보다 오래 막히거나(호출자가 읽지 않음), 쓰기·직렬화가 실패하면(호출자가 사라짐,
/// 상한을 넘는 비결과 프레임) 전송로를 죽은 것으로 확정한다(되돌리지 않는다). 그 뒤로는 아무것도 쓰지 않고
/// 프레임을 조용히 버리거나 잘라 보낸 뒤 이어 가지 않는다 — 기다리던 쪽은 즉시 풀리고, 실행기는 새 요청을 실행하지
/// 않은 채 [`EXIT_TRANSPORT`]로 끝난다. 프로세스가 프레임 쓰기 도중 끝나면 마지막 줄이 개행 없이 잘릴 수 있고
/// 그 뒤는 EOF뿐이다. 호출자는 개행으로 끝나지 않은 마지막 줄을 전송로 실패로 다뤄야 한다.
#[derive(Debug)]
struct Outbox {
    state: Mutex<OutState>,
    changed: Condvar,
    write_stall: Duration,
}

#[derive(Debug, Default)]
struct OutState {
    queue: VecDeque<Vec<u8>>,
    bytes: usize,
    /// 쓰기 스레드가 지금 쓰고 있는 프레임을 꺼낸 시각.
    writing_since: Option<Instant>,
    closed: bool,
    dead: bool,
}

impl OutState {
    fn check_stall(&mut self, write_stall: Duration) {
        if self
            .writing_since
            .is_some_and(|since| since.elapsed() >= write_stall)
        {
            self.dead = true;
        }
    }
}

impl Outbox {
    fn new(write_stall: Duration) -> Self {
        Self {
            state: Mutex::new(OutState::default()),
            changed: Condvar::new(),
            write_stall,
        }
    }

    /// `need`바이트 프레임 하나가 들어갈 자리가 날 때까지 기다린다. 전송로가 죽으면 `None`.
    /// 기다리는 동안 잠금을 놓는다.
    fn wait_room<'a>(
        &'a self,
        mut state: MutexGuard<'a, OutState>,
        need: usize,
    ) -> Option<MutexGuard<'a, OutState>> {
        loop {
            state.check_stall(self.write_stall);
            if state.dead {
                self.changed.notify_all();
                return None;
            }
            if state.queue.len() < OUT_QUEUE_FRAMES && state.bytes + need <= OUT_QUEUE_BYTES {
                return Some(state);
            }
            // 시간 제한은 바뀜이 없어도 정체를 다시 판정하려는 것이다.
            state = self
                .changed
                .wait_timeout(state, IDLE_POLL)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }

    fn enqueue(&self, mut state: MutexGuard<'_, OutState>, line: Option<Vec<u8>>) {
        match line {
            Some(line) => {
                if let Some(mut state) = self.wait_room(state, line.len()) {
                    state.bytes += line.len();
                    state.queue.push_back(line);
                }
            }
            None => state.dead = true,
        }
        self.changed.notify_all();
    }

    /// 프레임 하나를 넣는다. 자리가 날 때까지 기다리며, 전송로가 죽었으면 아무것도 하지 않는다.
    fn send(&self, frame: &Outbound) {
        // 큰 결과는 잠금 밖에서 직렬화한다.
        let line = encode(frame);
        self.enqueue(guard(&self.state), line);
    }

    /// 자리를 먼저 확보한 잠금 안에서 `make`로 답을 만들어 넣는다. 그 사이 다른 프레임이 끼지 않으므로
    /// `make`가 적재한 요청의 `result`보다 답이 먼저 나간다. 전송로가 죽었으면 `make`를 부르지 않는다.
    fn reply(&self, make: impl FnOnce() -> Outbound) {
        let Some(state) = self.wait_room(guard(&self.state), REPLY_ROOM) else {
            return;
        };
        let line = encode(&make());
        self.enqueue(state, line);
    }

    fn alive(&self) -> bool {
        let mut state = guard(&self.state);
        state.check_stall(self.write_stall);
        if state.dead {
            self.changed.notify_all();
        }
        !state.dead
    }

    /// 쓰기 스레드 본체. 실패·정체로 죽으면 남은 프레임을 쓰지 않고 돌아간다.
    fn run_writer(&self, out: &mut impl Write) {
        loop {
            let line = {
                let mut state = guard(&self.state);
                loop {
                    if state.dead || (state.closed && state.queue.is_empty()) {
                        return;
                    }
                    if let Some(line) = state.queue.pop_front() {
                        state.writing_since = Some(Instant::now());
                        break line;
                    }
                    state = self
                        .changed
                        .wait(state)
                        .unwrap_or_else(PoisonError::into_inner);
                }
            };
            let written = out.write_all(&line).and_then(|()| out.flush()).is_ok();
            let mut state = guard(&self.state);
            state.writing_since = None;
            state.bytes -= line.len();
            if !written {
                state.dead = true;
            }
            self.changed.notify_all();
        }
    }

    /// 새 프레임을 받지 않고 남은 것을 `timeout`까지 내보낸다. 모두 썼으면 `true`.
    fn finish(&self, timeout: Duration) -> bool {
        let mut state = guard(&self.state);
        state.closed = true;
        self.changed.notify_all();
        let deadline = Instant::now() + timeout;
        while !state.dead && (!state.queue.is_empty() || state.writing_since.is_some()) {
            let now = Instant::now();
            if now >= deadline {
                state.dead = true;
                break;
            }
            state = self
                .changed
                .wait_timeout(state, (deadline - now).min(IDLE_POLL))
                .unwrap_or_else(PoisonError::into_inner)
                .0;
            state.check_stall(self.write_stall);
        }
        self.changed.notify_all();
        !state.dead
    }
}

/// 워커를 돌린다. 반환값은 프로세스 종료 코드(0 정상 종료, 1 초기화 실패, 2 프로토콜 오류,
/// [`EXIT_TRANSPORT`] 출력 전송로 실패)다. 전송로 실패면 리더 스레드를 기다리지 않는다(stdin에 막혀 있을 수 있다).
/// 프로세스는 `main`이 돌아가면 끝난다.
pub fn serve<R, W, B>(input: R, output: W, identity: Identity, config: Config, backend: B) -> u8
where
    R: BufRead + Send + 'static,
    W: Write + Send + 'static,
    B: Backend,
{
    let shared = Arc::new(Shared {
        ledger: Mutex::new(Ledger::new(LEDGER_CAPACITY)),
        outbox: Outbox::new(config.write_stall),
    });
    let writer_shared = Arc::clone(&shared);
    if std::thread::Builder::new()
        .name("computer-worker-writer".into())
        .spawn(move || {
            let mut output = output;
            writer_shared.outbox.run_writer(&mut output);
        })
        .is_err()
    {
        return 1;
    }
    shared.outbox.send(&Outbound::Hello {
        contract: CONTRACT,
        epoch: identity.epoch.clone(),
        context: identity.context.clone(),
        pid: std::process::id(),
        supported: backend.supported(),
        capabilities: backend.capabilities(),
        unsupported: backend.unsupported(),
        limits: Limits::current(),
    });
    let (jobs, queue) = mpsc::sync_channel::<Job>(QUEUE_CAPACITY);
    let reader_shared = Arc::clone(&shared);
    let reader_identity = identity;
    let Ok(reader) = std::thread::Builder::new()
        .name("computer-worker-reader".into())
        .spawn(move || read_loop(input, &jobs, &reader_shared, &reader_identity))
    else {
        return 1;
    };
    let mut executor = Executor {
        backend,
        lock_path: config.lock_path,
        quiescing: None,
    };
    loop {
        let job = queue.recv_timeout(IDLE_POLL);
        if matches!(job, Err(RecvTimeoutError::Disconnected)) {
            break;
        }
        // 쉬는 동안에도 정체를 판정한다. 전송로가 죽었으면 받은 요청도 실행하지 않는다(결과를 알릴 수 없다).
        if !shared.outbox.alive() {
            return EXIT_TRANSPORT;
        }
        let Ok(job) = job else { continue };
        let outcome = executor.execute(&job, &shared);
        guard(&shared.ledger).settle(&outcome);
        shared.outbox.send(&Outbound::Result(Box::new(outcome)));
    }
    if !shared.outbox.finish(FLUSH_TIMEOUT) {
        return EXIT_TRANSPORT;
    }
    reader.join().unwrap_or(1)
}

fn read_loop<R: BufRead>(
    mut input: R,
    jobs: &SyncSender<Job>,
    shared: &Shared,
    identity: &Identity,
) -> u8 {
    let mut buf = Vec::new();
    let code = loop {
        let parsed = match read_frame(&mut input, &mut buf) {
            Ok(true) => parse_line(&buf),
            Ok(false) => break 0,
            Err(error) => Err(error.to_string()),
        };
        if !shared.outbox.alive() {
            break EXIT_TRANSPORT;
        }
        let inbound = match parsed {
            Ok(Parsed::Inbound(inbound)) => inbound,
            Ok(Parsed::Blank) => continue,
            // 계약 밖 요청(미지원 op 등): seq를 예약하지 않고 거절한다. 줄 경계는 온전하므로 계속 읽는다.
            Ok(Parsed::Rejected { seq, message }) => {
                shared.outbox.send(&Outbound::Rejected {
                    seq,
                    code: RejectCode::InvalidRequest,
                    message,
                });
                continue;
            }
            // 상한 초과·JSON 아님: 재동기화하지 않고 끝낸다.
            Err(message) => {
                shared.outbox.send(&Outbound::ProtocolError {
                    message: diagnostic(message),
                });
                break 2;
            }
        };
        match inbound {
            Inbound::Request(frame) => admit(frame, jobs, shared, identity),
            Inbound::Status { seq } => {
                let (state, summary) = guard(&shared.ledger).status(seq);
                shared.outbox.send(&Outbound::Status {
                    seq,
                    state,
                    summary,
                });
            }
            Inbound::Cancel { seq } => {
                let outcome = guard(&shared.ledger).cancel(seq);
                shared.outbox.send(&Outbound::Cancel { seq, outcome });
            }
            Inbound::Shutdown => break 0,
        }
    };
    // 발행 전 요청은 미실행으로 확정한다. 실행기는 이를 보고 부작용 없이 결과만 낸다.
    guard(&shared.ledger).cancel_all_queued();
    code
}

fn admit(frame: RequestFrame, jobs: &SyncSender<Job>, shared: &Shared, identity: &Identity) {
    let seq = frame.seq;
    let reject = |code: RejectCode, message: String| {
        shared.outbox.send(&Outbound::Rejected {
            seq: Some(seq),
            code,
            message,
        });
    };
    if frame.epoch != identity.epoch {
        return reject(
            RejectCode::StaleEpoch,
            "request targets another executor instance".to_owned(),
        );
    }
    if frame.context != identity.context {
        return reject(
            RejectCode::ForeignContext,
            "context was not issued to this caller".to_owned(),
        );
    }
    if frame.deadline_ms == 0 || frame.deadline_ms > MAX_DEADLINE_MS {
        return reject(
            RejectCode::InvalidRequest,
            format!("deadlineMs must be 1..={MAX_DEADLINE_MS}"),
        );
    }
    if let Err(message) = frame.op.validate() {
        return reject(RejectCode::InvalidRequest, message);
    }
    let deadline = Instant::now() + Duration::from_millis(frame.deadline_ms);
    let digest = frame.digest();
    // 출력 자리를 확보한 잠금 안에서 접수·적재해 `accepted`가 그 요청의 `result`보다 먼저 나가게 한다.
    shared.outbox.reply(|| {
        let admission = guard(&shared.ledger).admit(seq, digest);
        match admission {
            Admission::New => match jobs.try_send(Job { frame, deadline }) {
                Ok(()) => Outbound::Accepted { seq },
                Err(error) => {
                    guard(&shared.ledger).finish(seq, RequestState::NotDispatched);
                    let (code, message) = match error {
                        TrySendError::Full(_) => (RejectCode::Busy, "executor queue is full"),
                        TrySendError::Disconnected(_) => {
                            (RejectCode::ShuttingDown, "executor is shutting down")
                        }
                    };
                    Outbound::Rejected {
                        seq: Some(seq),
                        code,
                        message: message.to_owned(),
                    }
                }
            },
            Admission::Duplicate(state) => Outbound::Duplicate { seq, state },
            Admission::PayloadMismatch => rejected(
                seq,
                RejectCode::PayloadMismatch,
                "seq was already used for a different request",
            ),
            Admission::Expired => rejected(
                seq,
                RejectCode::Expired,
                "seq is older than this context's window; it is never re-run",
            ),
            Admission::OutOfRange => rejected(
                seq,
                RejectCode::SeqOutOfRange,
                "seq must be greater than the last seq and within the allowed gap",
            ),
            Admission::Busy => rejected(seq, RejectCode::Busy, "too many requests in flight"),
        }
    });
}

fn rejected(seq: u64, code: RejectCode, message: &str) -> Outbound {
    Outbound::Rejected {
        seq: Some(seq),
        code,
        message: message.to_owned(),
    }
}

struct Executor<B> {
    backend: B,
    lock_path: PathBuf,
    /// 결과 불명·정리 미완료 뒤 쥐고 있는 입력 잠금. 있으면 변경 요청을 받지 않는다.
    quiescing: Option<InputLock>,
}

impl<B: Backend> Executor<B> {
    fn execute(&mut self, job: &Job, shared: &Shared) -> Outcome {
        let seq = job.frame.seq;
        let state = || guard(&shared.ledger).state(seq);
        if state() != RequestState::Queued {
            return Outcome::not_dispatched(
                seq,
                "cancelled",
                "request was cancelled before dispatch",
            );
        }
        if Instant::now() >= job.deadline {
            return deadline_exceeded(seq);
        }
        let op = &job.frame.op;
        if !op.is_mutation() {
            if !guard(&shared.ledger).begin_dispatch(seq) {
                return Outcome::not_dispatched(
                    seq,
                    "cancelled",
                    "request was cancelled before dispatch",
                );
            }
            return self.read(seq, op, job.deadline);
        }
        if self.quiescing.is_some() {
            return Outcome::not_dispatched(
                seq,
                "executor_quiescing",
                "a previous input has an unknown outcome; restart the worker before more input",
            );
        }
        let input = match lock::acquire(&self.lock_path, job.deadline, || {
            state() == RequestState::Queued
        }) {
            Ok(input) => input,
            Err(error) => return Outcome::not_dispatched(seq, error.code(), error.message()),
        };
        if let Op::Batch { steps } = op {
            return self.batch(seq, steps, job.deadline, input, shared);
        }
        let dispatched = match self.step(op, job.deadline, || {
            guard(&shared.ledger).begin_dispatch(seq)
        }) {
            Ok(dispatched) => dispatched,
            Err(refusal) => return Outcome::not_dispatched(seq, refusal.code, refusal.message),
        };
        let state = Outcome::state_for(&dispatched.delivery, &dispatched.cleanup);
        if state == RequestState::Unknown {
            self.quiescing = Some(input);
        }
        Outcome {
            seq,
            state,
            delivery: dispatched.delivery,
            observation: Observation::NotRequested,
            verification: dispatched.verification,
            cleanup: dispatched.cleanup,
            error: None,
            data: dispatched.data,
        }
    }

    /// 한 단계: 기한 → 재검증 → 기한 → 발행 문(`gate`) → 발행. 발행하지 않았으면 그 이유.
    fn step(
        &mut self,
        op: &Op,
        deadline: Instant,
        gate: impl FnOnce() -> bool,
    ) -> Result<Dispatched, Refusal> {
        let late = || {
            Refusal::new(
                "deadline_exceeded",
                "request deadline passed before dispatch",
            )
        };
        if Instant::now() >= deadline {
            return Err(late());
        }
        let prepared = self.backend.prepare(op, deadline)?;
        if Instant::now() >= deadline {
            return Err(late());
        }
        if !gate() {
            return Err(Refusal::new(
                "cancelled",
                "request was cancelled before dispatch",
            ));
        }
        Ok(self.backend.dispatch(prepared, deadline))
    }

    /// 잠금 `input`을 쥔 채 단계를 차례로 실행한다. 결과 축은 단계 합이고 `data.steps`에 단계별 축이 있다.
    fn batch(
        &mut self,
        seq: u64,
        steps: &[Op],
        deadline: Instant,
        input: InputLock,
        shared: &Shared,
    ) -> Outcome {
        let mut entries = Vec::with_capacity(steps.len());
        let mut units = 0u32;
        let mut issued_steps = 0u32;
        // 전달이 `submitted`인 단계 수. 모든 단계가 그래야 결과도 `submitted`다.
        let mut submitted = 0usize;
        let mut unknown = None;
        let mut verification = Verification::NotRequested;
        let mut cleanup = Cleanup::NotNeeded;
        // 멈춘 단계와 이유. 발행하지 못한 단계면 그 거절, 발행한 단계면 축에서 읽은 이유다.
        let mut stop: Option<(usize, ErrorInfo)> = None;
        for (index, op) in steps.iter().enumerate() {
            let result = if index > 0 && guard(&shared.ledger).stop_requested(seq) {
                Err(Refusal::new(
                    "cancelled",
                    "request was cancelled before this step",
                ))
            } else {
                self.step(op, deadline, || {
                    if index == 0 {
                        guard(&shared.ledger).begin_dispatch(seq)
                    } else {
                        !guard(&shared.ledger).stop_requested(seq)
                    }
                })
            };
            let dispatched = match result {
                Ok(dispatched) => dispatched,
                Err(refusal) => {
                    let error = ErrorInfo {
                        code: refusal.code.to_owned(),
                        message: diagnostic(refusal.message),
                    };
                    entries.push(serde_json::json!({
                        "index": index,
                        "delivery": Delivery::NotDispatched { reason: error.code.clone() },
                        "verification": Verification::NotRequested,
                        "cleanup": Cleanup::NotNeeded,
                        "error": error,
                    }));
                    stop = Some((index, error));
                    break;
                }
            };
            let halt = match &dispatched.delivery {
                Delivery::Submitted { issued_units } => {
                    units = units.saturating_add(*issued_units);
                    issued_steps += 1;
                    submitted += 1;
                    None
                }
                Delivery::Partial {
                    issued_units,
                    reason,
                } => {
                    units = units.saturating_add(*issued_units);
                    issued_steps += 1;
                    Some(reason.clone())
                }
                Delivery::Unknown { reason } => {
                    issued_steps += 1;
                    unknown = Some(reason.clone());
                    Some(reason.clone())
                }
                Delivery::NotDispatched { reason } => Some(reason.clone()),
                Delivery::ReadOnly => Some("not_an_input".to_owned()),
            };
            let halt = halt
                .or_else(|| match &dispatched.verification {
                    Verification::Unsatisfied { .. } => Some("verification_unsatisfied".to_owned()),
                    Verification::Unknown { .. } => Some("verification_unknown".to_owned()),
                    Verification::NotRequested | Verification::Satisfied { .. } => None,
                })
                .or_else(|| {
                    matches!(dispatched.cleanup, Cleanup::Incomplete { .. })
                        .then(|| "cleanup_incomplete".to_owned())
                });
            let mut entry = serde_json::json!({
                "index": index,
                "delivery": dispatched.delivery,
                "verification": dispatched.verification,
                "cleanup": dispatched.cleanup,
            });
            if dispatched.verification != Verification::NotRequested {
                verification = dispatched.verification;
            }
            if dispatched.cleanup != Cleanup::NotNeeded {
                cleanup = dispatched.cleanup;
            }
            if let Some(code) = halt {
                let error = ErrorInfo {
                    message: diagnostic(format!("step {index} did not complete: {code}")),
                    code: diagnostic(code),
                };
                entry["error"] = serde_json::json!(error);
                entries.push(entry);
                stop = Some((index, error));
                break;
            }
            entries.push(entry);
        }
        for index in entries.len()..steps.len() {
            entries.push(serde_json::json!({
                "index": index,
                "delivery": Delivery::NotDispatched { reason: BATCH_STOPPED.to_owned() },
                "verification": Verification::NotRequested,
                "cleanup": Cleanup::NotNeeded,
            }));
        }
        let reason = stop
            .as_ref()
            .map_or_else(String::new, |(_, error)| error.code.clone());
        let delivery = if let Some(reason) = unknown {
            Delivery::Unknown { reason }
        } else if submitted == steps.len() {
            Delivery::Submitted {
                issued_units: units,
            }
        } else if issued_steps == 0 {
            // 아무것도 내지 못했다: 단일 요청과 같은 미실행 결과다.
            Delivery::NotDispatched {
                reason: reason.clone(),
            }
        } else {
            Delivery::Partial {
                issued_units: units,
                reason: reason.clone(),
            }
        };
        // 모든 단계가 제출됐으면 마지막 단계의 검증·정리 축이 결과를 말한다.
        let error = stop.and_then(|(index, error)| match &delivery {
            Delivery::Submitted { .. } => None,
            Delivery::NotDispatched { .. } => Some(error),
            _ => Some(ErrorInfo {
                code: BATCH_STOPPED.to_owned(),
                message: diagnostic(format!("batch stopped at step {index}: {reason}")),
            }),
        });
        let state = Outcome::state_for(&delivery, &cleanup);
        if state == RequestState::Unknown {
            self.quiescing = Some(input);
        }
        Outcome {
            seq,
            state,
            delivery,
            observation: Observation::NotRequested,
            verification,
            cleanup,
            error,
            data: Some(serde_json::json!({
                "issuedSteps": issued_steps,
                "steps": entries,
            })),
        }
    }

    fn read(&mut self, seq: u64, op: &Op, deadline: Instant) -> Outcome {
        let result = if matches!(op, Op::Capabilities) {
            Ok(ReadResult {
                observation: Observation::NotRequested,
                data: serde_json::json!({
                    "contract": CONTRACT,
                    "supported": self.backend.supported(),
                    "capabilities": self.backend.capabilities(),
                    "unsupported": self.backend.unsupported(),
                    "limits": Limits::current(),
                    "inputLock": self.lock_path.display().to_string(),
                    "quiescing": self.quiescing.is_some(),
                }),
            })
        } else {
            self.backend.read(op, deadline)
        };
        let (observation, error, data) = match result {
            Ok(read) => (read.observation, None, Some(read.data)),
            Err(refusal) => (
                Observation::Failed {
                    reason: refusal.code.to_owned(),
                },
                Some(ErrorInfo {
                    code: refusal.code.to_owned(),
                    message: refusal.message,
                }),
                None,
            ),
        };
        Outcome {
            seq,
            state: RequestState::Completed,
            delivery: Delivery::ReadOnly,
            observation,
            verification: Verification::NotRequested,
            cleanup: Cleanup::NotNeeded,
            error,
            data,
        }
    }
}

fn deadline_exceeded(seq: u64) -> Outcome {
    Outcome::not_dispatched(
        seq,
        "deadline_exceeded",
        "request deadline passed before dispatch",
    )
}

#[cfg(test)]
mod tests;
