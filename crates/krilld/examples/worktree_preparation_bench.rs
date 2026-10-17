//! Reproduce MK-095 with a synthetic 36,000-file repository, entirely under a temporary directory.
//! Build krilld and this example, then run `target/debug/examples/worktree_preparation_bench`.
//! `--expiry` also measures production five-minute collection. Timings include the daemon IPC roundtrip.
use std::fs;
use std::io::{self, Write as _};
use std::os::unix::ffi::OsStrExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use krill_proto::control::{
    NewWorktree, PrepareWorktree, Reply, Request, Setup, WorktreePreparation,
};
use krill_proto::daemon::{self, client_hello};
use krill_proto::ipc::{self, ClientRole};
use krill_proto::screen::Msg as _;
use krilld::git::worktree::prepare::TTL;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
struct Temp(PathBuf);
impl Drop for Temp {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).ok();
    }
}

struct Daemon {
    child: Child,
    socket: PathBuf,
}

impl Drop for Daemon {
    fn drop(&mut self) {
        self.child.kill().ok();
        self.child.wait().ok();
    }
}

impl Daemon {
    fn start(temp: &Path, root: &Path) -> Result<Self> {
        let binary = std::env::current_exe()?
            .parent()
            .and_then(Path::parent)
            .ok_or("cannot find sibling krilld binary")?
            .join("krilld");
        let socket = temp.join("state/run/krilld.sock");
        let daemon = Self {
            child: Command::new(binary)
                .arg(&socket)
                .env("KRILL_STATE_DIR", temp.join("state"))
                .env("KRILL_LOG_DIR", temp.join("logs"))
                .env("KRILL_WORKTREES_DIR", root)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()?,
            socket,
        };
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if daemon.request(&Request::List).is_ok() {
                return Ok(daemon);
            }
            if Instant::now() >= deadline {
                return Err("benchmark daemon did not start".into());
            }
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn request(&self, req: &Request) -> Result<Reply> {
        let mut connection = daemon::handshake(
            &self.socket,
            &client_hello(ClientRole::App, "mk095-bench"),
            Duration::from_secs(30),
        )?;
        connection.stream.write_all(&req.encode()?)?;
        let (kind, bytes) =
            ipc::read_frame(&mut connection.stream)?.ok_or("benchmark daemon disconnected")?;
        Ok(Reply::decode(kind, &bytes)?)
    }
}

fn git(repo: &Path, args: &[&str]) -> Result<()> {
    let output = Command::new("git")
        .args([
            "-c",
            "user.name=MK095",
            "-c",
            "user.email=mk095@localhost",
            "-c",
            "core.hooksPath=/dev/null",
        ])
        .args(args)
        .current_dir(repo)
        .output()?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).into_owned().into());
    }
    Ok(())
}

fn ready(daemon: &Daemon, req: &PrepareWorktree) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(90);
    loop {
        match daemon.request(&Request::PrepareWorktree(req.clone()))? {
            Reply::WorktreePreparation(WorktreePreparation::Ready) => return Ok(()),
            Reply::WorktreePreparation(
                WorktreePreparation::Preparing | WorktreePreparation::Busy,
            ) if Instant::now() < deadline => {
                thread::sleep(Duration::from_millis(10));
            }
            other => return Err(format!("preparation failed: {other:?}").into()),
        }
    }
}

fn main() -> Result<()> {
    let expiry = std::env::args().any(|a| a == "--expiry");
    let temp = Temp(Path::new("/tmp").join(format!(
        "mk095-bench-{}-{}",
        std::process::id(),
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
    )));
    let repo = temp.0.join("repo");
    fs::create_dir_all(&repo)?;
    for n in 0..36_000 {
        let dir = repo.join(format!("dir-{}", n / 1000));
        fs::create_dir_all(&dir)?;
        fs::write(
            dir.join(format!("file-{n}.txt")),
            format!("MK-095 deterministic fixture file {n}\n"),
        )?;
    }
    git(&repo, &["init", "-q", "-b", "main"])?;
    git(&repo, &["add", "."])?;
    git(&repo, &["commit", "-qm", "36k files"])?;
    let root = temp.0.join("worktrees");
    let daemon = Daemon::start(&temp.0, &root)?;
    println!(
        "files=36000 ttl_seconds={} disk_limit_bytes={}",
        TTL.as_secs(),
        krilld::git::worktree::prepare::DISK_LIMIT
    );
    for n in 0..4 {
        let req = NewWorktree {
            repo: repo.as_os_str().as_bytes().to_vec(),
            name: format!("agent-{n}"),
            base: None,
            setup: Setup::Skip,
        };
        if n != 0 {
            let prep = PrepareWorktree {
                token: n,
                repo: req.repo.clone(),
                base: None,
                cancel: false,
            };
            let start = Instant::now();
            ready(&daemon, &prep)?;
            println!(
                "sample={n} preparation_ms={:.3}",
                start.elapsed().as_secs_f64() * 1000.0
            );
        }
        let start = Instant::now();
        let outcome = daemon.request(&Request::NewWorktree(req))?;
        let elapsed = start.elapsed();
        let Reply::WorktreeCreated(worktree) = outcome else {
            return Err(format!("creation did not finish: {outcome:?}").into());
        };
        let path = PathBuf::from(std::ffi::OsStr::from_bytes(&worktree.path));
        if !path.join("dir-35/file-35999.txt").is_file() {
            return Err("incomplete checkout".into());
        }
        println!(
            "sample={n} prepared={} creation_ms={:.3}",
            n != 0,
            elapsed.as_secs_f64() * 1000.0
        );
        io::stdout().flush()?;
        if n != 0 && elapsed >= Duration::from_secs(1) {
            return Err("prepared creation exceeded one second".into());
        }
    }
    if expiry {
        let req = PrepareWorktree {
            token: 100,
            repo: repo.as_os_str().as_bytes().to_vec(),
            base: None,
            cancel: false,
        };
        let start = Instant::now();
        ready(&daemon, &req)?;
        println!("expiry_wait_started=true");
        io::stdout().flush()?;
        let deadline = start + TTL + Duration::from_secs(30);
        while root.join(".minikrill-preparing/owner.json").exists() {
            if Instant::now() >= deadline {
                return Err("unused preparation was not collected".into());
            }
            thread::sleep(Duration::from_millis(20));
        }
        let list = Command::new("git")
            .args(["worktree", "list", "--porcelain"])
            .current_dir(&repo)
            .output()?;
        if String::from_utf8_lossy(&list.stdout).contains(".minikrill-preparing") {
            return Err("expired git registration remains".into());
        }
        println!(
            "unused_cleanup_ms={:.3} registration_removed=true",
            start.elapsed().as_secs_f64() * 1000.0
        );
    }
    Ok(())
}
