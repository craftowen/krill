//! ssh.exe에 넘기는 끝도 overlapped여야 한다(OWE-915 VM 실측).
use std::fs::OpenOptions;
use std::io::{self, Read, Write};
use std::os::windows::fs::OpenOptionsExt as _;
use std::os::windows::process::CommandExt as _;
use std::process::{Child, Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock, mpsc};
use std::thread;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};
use tokio::runtime::Runtime;

const FILE_FLAG_OVERLAPPED: u32 = 0x4000_0000;
static SEQUENCE: AtomicU64 = AtomicU64::new(0);
static RUNTIME: OnceLock<Arc<Runtime>> = OnceLock::new();

fn runtime() -> io::Result<Arc<Runtime>> {
    if let Some(rt) = RUNTIME.get() {
        return Ok(Arc::clone(rt));
    }
    let rt = Arc::new(
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()?,
    );
    RUNTIME.set(Arc::clone(&rt)).ok();
    Ok(RUNTIME.get().map_or(rt, Arc::clone))
}

pub(super) struct Pipe {
    io: NamedPipeServer,
    runtime: Arc<Runtime>,
}
impl std::fmt::Debug for Pipe {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pipe").finish_non_exhaustive()
    }
}
impl Pipe {
    fn pair() -> io::Result<(Self, std::fs::File)> {
        let runtime = runtime()?;
        let name = format!(
            r"\\.\pipe\krill-ssh-{}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        );
        let server = {
            let _entered = runtime.enter();
            ServerOptions::new()
                .first_pipe_instance(true)
                .max_instances(1)
                .reject_remote_clients(true)
                .create(&name)?
        };
        // 읽기·쓰기 모두 요구해 기본 DACL의 Everyone 읽기 권한만으로 열 수 없게 한다.
        // 다른 프로세스가 먼저 연결하면 이 open이 실패하며 전송 전에 양쪽 끝을 닫는다.
        let child = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(FILE_FLAG_OVERLAPPED)
            .open(&name)?;
        let (tx, rx) = mpsc::sync_channel(1);
        runtime.spawn(async move {
            let result = server.connect().await.map(|()| server);
            tx.send(result).ok();
        });
        let io = rx.recv().map_err(io::Error::other)??;
        Ok((Self { io, runtime }, child))
    }
}
// 이 타입의 read/write는 전용 OS 스레드에서만 호출한다. 호출자의 tokio 런타임 안에서 block_on하지 않는다.
impl Read for Pipe {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        self.runtime.block_on(self.io.read(bytes))
    }
}
impl Write for Pipe {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.runtime.block_on(self.io.write(bytes))
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[derive(Debug)]
pub(super) struct ChildGuard(pub(super) Child);
impl ChildGuard {
    pub(super) fn stop(&mut self) {
        self.0.kill().ok();
        self.0.wait().ok();
    }
}
impl Drop for ChildGuard {
    fn drop(&mut self) {
        self.stop();
    }
}

pub(super) struct Process {
    pub(super) child: ChildGuard,
    pub(super) stdin: Pipe,
    pub(super) stdout: Pipe,
    pub(super) stderr: Pipe,
}
pub(super) fn spawn(program: &std::ffi::OsStr, args: &[String]) -> io::Result<Process> {
    let (stdin, input) = Pipe::pair()?;
    let (stdout, output) = Pipe::pair()?;
    let (stderr, error) = Pipe::pair()?;
    let child = Command::new(program)
        .creation_flags(0x0800_0000)
        .args(args)
        .stdin(input)
        .stdout(output)
        .stderr(error)
        .spawn()?;
    Ok(Process {
        child: ChildGuard(child),
        stdin,
        stdout,
        stderr,
    })
}

pub(crate) fn output(
    program: &std::ffi::OsStr,
    args: &[String],
    bytes: Option<&[u8]>,
    timeout: Duration,
) -> io::Result<Output> {
    let Process {
        mut child,
        mut stdin,
        mut stdout,
        mut stderr,
    } = spawn(program, args)?;
    thread::scope(|scope| {
        let input = scope.spawn(move || -> io::Result<()> {
            if let Some(bytes) = bytes {
                stdin.write_all(bytes)?;
            }
            Ok(())
        });
        let output = scope.spawn(move || {
            let mut bytes = Vec::new();
            stdout.read_to_end(&mut bytes).map(|_| bytes)
        });
        let error = scope.spawn(move || {
            let mut bytes = Vec::new();
            stderr.read_to_end(&mut bytes).map(|_| bytes)
        });
        let deadline = Instant::now() + timeout;
        let status = loop {
            match child.0.try_wait() {
                Ok(Some(status)) => break Ok(status),
                Err(e) => break Err(e),
                Ok(None) if Instant::now() >= deadline => {
                    break Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "ssh command timed out",
                    ));
                }
                Ok(None) => thread::sleep(Duration::from_millis(20)),
            }
        };
        if status.is_err() {
            child.stop();
        }
        let stdin_result = input
            .join()
            .map_err(|_| io::Error::other("ssh input worker panicked"))?;
        let stdout = output
            .join()
            .map_err(|_| io::Error::other("ssh output worker panicked"))??;
        let stderr = error
            .join()
            .map_err(|_| io::Error::other("ssh error worker panicked"))??;
        let status = status?;
        // 원격 실패 상세를 stdin BrokenPipe보다 우선한다.
        if status.success() {
            stdin_result?;
        }
        Ok(Output {
            status,
            stdout,
            stderr,
        })
    })
}
