//! Console probes run in a separate, bounded helper: attaching the daemon itself
//! would change process-global console state and race other sessions.

use std::collections::BTreeMap;
use std::io::{self, Read as _, Write as _};
use std::os::windows::io::AsHandle as _;
use std::process::{Command, ExitCode, Stdio};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

type Probe = (u64, Instant, Vec<(u32, u64)>);
static CACHE: OnceLock<Mutex<BTreeMap<u32, Probe>>> = OnceLock::new();

pub(crate) fn session_agent_process(root: u32) -> Option<u32> {
    let tree = super::processes(None)
        .ok()?
        .into_iter()
        .map(|process| (process.pid, process.parent, process.exe))
        .collect::<Vec<_>>();
    if !crate::status::process_tree::has_session_agent(root, &tree) {
        return None;
    }
    let root_start = super::start_us(root).ok()?;
    let mut cache = CACHE.get_or_init(Mutex::default).lock().ok()?;
    cache.retain(|_, (_, at, _)| at.elapsed() < Duration::from_millis(500));
    let members = if let Some((_, _, members)) = cache
        .get(&root)
        .filter(|(start, _, _)| *start == root_start)
    {
        members.clone()
    } else {
        let members: Vec<_> = probe(root)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|pid| super::start_us(pid).ok().map(|start| (pid, start)))
            .collect();
        cache.insert(root, (root_start, Instant::now(), members.clone()));
        members
    };
    if !members.contains(&(root, root_start)) {
        return None;
    }
    let candidate = crate::status::process_tree::session_agent_in_console(
        root,
        &tree,
        |pid| super::start_us(pid).ok(),
        |pid| super::start_us(pid).is_ok_and(|start| members.contains(&(pid, start))),
    )?;
    (super::start_us(root).ok() == Some(root_start)).then_some(candidate)
}

fn probe(root: u32) -> io::Result<Vec<u32>> {
    let mut command = Command::new(std::env::current_exe()?);
    command
        .args(["--console-members", &root.to_string()])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = crate::child::spawn(&mut command)?;
    let output = child.stdout.take().ok_or(io::ErrorKind::BrokenPipe)?;
    let mut output = crate::child::PipeReader::new(output)?;
    let mut bytes = Vec::new();
    let deadline = Instant::now() + Duration::from_millis(250);
    loop {
        let mut buffer = [0; 4096];
        match output.read(&mut buffer) {
            Ok(0) => {}
            Ok(n) => bytes.extend_from_slice(buffer.get(..n).ok_or(io::ErrorKind::InvalidData)?),
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
            Err(e) => return Err(e),
        }
        if bytes.len() > 64 * 1024 {
            return Err(io::ErrorKind::InvalidData.into());
        }
        if let Some(status) = child.try_wait()? {
            if !status.success() {
                return Err(io::ErrorKind::NotFound.into());
            }
            // The finite helper writes less than the cap before exiting; drain the pipe.
            output.read_to_end(&mut bytes)?;
            if bytes.len() > 64 * 1024 {
                return Err(io::ErrorKind::InvalidData.into());
            }
            return String::from_utf8(bytes)
                .map_err(|_| io::ErrorKind::InvalidData)?
                .lines()
                .map(|line| line.parse().map_err(|_| io::ErrorKind::InvalidData.into()))
                .collect();
        }
        if Instant::now() >= deadline {
            return Err(io::ErrorKind::TimedOut.into());
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

pub fn run_probe(pid: u32) -> ExitCode {
    // AttachConsole may replace standard handles. Pin the inherited output pipe
    // before attachment, so probing never writes PID text into the user's terminal.
    let output = io::stdout().as_handle().try_clone_to_owned();
    match (output, console_members(pid)) {
        (Ok(output), Ok(members)) => {
            let mut output = std::fs::File::from(output);
            for member in members {
                if writeln!(output, "{member}").is_err() {
                    return ExitCode::FAILURE;
                }
            }
            ExitCode::SUCCESS
        }
        _ => ExitCode::FAILURE,
    }
}

fn console_members(pid: u32) -> io::Result<Vec<u32>> {
    use windows_sys::Win32::System::Console::{AttachConsole, FreeConsole, GetConsoleProcessList};
    // SAFETY: this dedicated helper owns no sessions and may detach its own console.
    unsafe { FreeConsole() };
    // SAFETY: attaches only this short-lived helper, never the daemon or holder.
    if unsafe { AttachConsole(pid) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut members = vec![0; 256];
    loop {
        let capacity = u32::try_from(members.len()).map_err(|_| io::ErrorKind::InvalidData)?;
        // SAFETY: members is writable for capacity u32 values.
        let count = unsafe { GetConsoleProcessList(members.as_mut_ptr(), capacity) };
        if count == 0 {
            return Err(io::Error::last_os_error());
        }
        if count > 4096 {
            return Err(io::ErrorKind::InvalidData.into());
        }
        if count <= capacity {
            members.truncate(usize::try_from(count).map_err(|_| io::ErrorKind::InvalidData)?);
            return Ok(members);
        }
        members.resize(
            usize::try_from(count).map_err(|_| io::ErrorKind::InvalidData)?,
            0,
        );
    }
}
