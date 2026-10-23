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
    fn pair(deadline: Instant) -> io::Result<(Self, std::fs::File)> {
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
        let io = rx
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .map_err(|error| io::Error::new(io::ErrorKind::TimedOut, error))??;
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
    spawn_until(program, args, Instant::now() + Duration::from_secs(15))
}

fn spawn_until(
    program: &std::ffi::OsStr,
    args: &[String],
    deadline: Instant,
) -> io::Result<Process> {
    let (stdin, input) = Pipe::pair(deadline)?;
    let (stdout, output) = Pipe::pair(deadline)?;
    let (stderr, error) = Pipe::pair(deadline)?;
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
    // Callers may already run inside Tokio. Only this control thread enters our IO runtime;
    // all inherited pipe operations below are async and cancelled by the same deadline.
    thread::scope(|scope| {
        scope
            .spawn(move || output_inner(program, args, bytes, timeout))
            .join()
            .map_err(|_| io::Error::other("SSH command worker stopped"))?
    })
}

fn output_inner(
    program: &std::ffi::OsStr,
    args: &[String],
    bytes: Option<&[u8]>,
    timeout: Duration,
) -> io::Result<Output> {
    let deadline = Instant::now() + timeout;
    let Process {
        mut child,
        mut stdin,
        stdout,
        stderr,
    } = spawn_until(program, args, deadline)?;
    let runtime = stdout.runtime.clone();
    let result = runtime.block_on(async {
        tokio::time::timeout(deadline.saturating_duration_since(Instant::now()), async {
            let input = async move {
                let result = if let Some(bytes) = bytes {
                    stdin.io.write_all(bytes).await
                } else {
                    Ok(())
                };
                drop(stdin);
                Ok::<_, io::Error>(result)
            };
            let output = capped_output(stdout);
            let error = capped_output(stderr);
            let status = async {
                loop {
                    if let Some(status) = child.0.try_wait()? {
                        return Ok::<_, io::Error>(status);
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            };
            let (status, input, stdout, stderr) = tokio::try_join!(status, input, output, error)?;
            if status.success() {
                input?;
            }
            Ok(Output {
                status,
                stdout,
                stderr,
            })
        })
        .await
        .map_err(|_| {
            io::Error::new(
                io::ErrorKind::TimedOut,
                "SSH command/stdio deadline exceeded; remote result may be unknown",
            )
        })?
    });
    // Cancelling the async scope drops overlapped IO handles; never join a blocked pipe reader.
    if result.is_err() {
        child.stop();
    }
    result
}

async fn capped_output(pipe: Pipe) -> io::Result<Vec<u8>> {
    capped_reader(pipe.io).await
}
async fn capped_reader(reader: impl tokio::io::AsyncRead + Unpin) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader.take(1024 * 1024 + 1).read_to_end(&mut bytes).await?;
    if bytes.len() > 1024 * 1024 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "SSH output exceeded limit; remote result may be unknown",
        ));
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn async_output_cap_and_descendant_like_open_writer_are_cancellable() {
        runtime().unwrap().block_on(async {
            let (reader, mut writer) = tokio::io::duplex(4096);
            let write =
                tokio::spawn(async move { writer.write_all(&vec![0_u8; 1024 * 1024 + 1]).await });
            assert_eq!(
                capped_reader(reader).await.unwrap_err().kind(),
                io::ErrorKind::InvalidData
            );
            write.await.unwrap().ok();
            let (reader, held_writer) = tokio::io::duplex(4096);
            let result =
                tokio::time::timeout(Duration::from_millis(20), capped_reader(reader)).await;
            assert!(
                result.is_err(),
                "an open inherited writer must not force a blocking join"
            );
            drop(held_writer);
        });
    }
}
