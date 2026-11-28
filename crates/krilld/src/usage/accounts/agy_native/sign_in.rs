// Adapted from Orca:
// - `src/main/antigravity/antigravity-chat-sign-in.ts`
// - `src/main/runtime/rpc/methods/accounts/antigravity-accounts.ts`
// (Copyright (c) 2026 Lovecast Inc., MIT License).
//! Execution-host `agy acp --login` sign-in flow manager.
//!
//! Runs one interactive OAuth sign-in per execution host:
//! - For `AddAccount`, saves any existing host login into the encrypted vault first so adding
//!   another Google account never loses the currently active login.
//! - Spawns `agy acp --login` under the host's native credential authority, reads stdout+stderr
//!   line-by-line for a strictly validated Google OAuth URL (`agy_login_link`), and writes one
//!   validated authorization code or callback URL (`agy_login_answer`) to stdin.
//! - Only the caller holding the random `flow_id` capability can read the OAuth URL, submit a
//!   code, or cancel the flow. Other callers observe only a redacted in-progress status.
//! - On success, evicts host/model caches and saves + selects the new account in the vault.

use std::collections::BTreeMap;
use std::io::{Read, Write as _};
use std::path::{Path, PathBuf};
use std::process::{ChildStdin, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use krill_proto::usage::{
    AntigravitySignInIntent, AntigravitySignInPhase, AntigravitySignInState, ProviderKind,
    agy_login_answer, agy_login_failure, agy_login_link,
};

use zeroize::Zeroizing;

use super::backend::{current_process_env, is_antigravity_file_storage_host, is_real_home};
use super::service::{AntigravityAccountService, canonical_host_key, generate_capability_uuid_v4};
use crate::session::lock;

const FLOW_DEADLINE: Duration = Duration::from_secs(300);
const CANCEL_WAIT_TIMEOUT: Duration = Duration::from_secs(3);
const OUTPUT_TAIL_BYTES: usize = 4_096;

const NOT_ACTIVE_ERR: &str = "This sign-in is no longer active in this client.";
const OTHER_CLIENT_ERR: &str = "Antigravity sign-in is already in progress in another window.";
const STILL_STOPPING_ERR: &str =
    "The last Antigravity sign-in is still stopping. Try again in a moment.";
const ALREADY_ANSWERED_ERR: &str =
    "That code was already sent. Wait for Antigravity to finish signing in.";
const WAIT_FOR_LINK_ERR: &str = "Wait for the Google sign-in link before you paste a code.";
const BAD_ANSWER_ERR: &str =
    "Paste the code Google shows after you sign in, or the address of that page, then try again.";
const MULTI_LINK_ERR: &str = "Antigravity started more than one Google sign-in request.";
const NOT_INSTALLED_ERR: &str =
    "agy is not installed on this host. Install it, then sign in again.";

type HostKey = (PathBuf, Option<PathBuf>);

static FLOWS: Mutex<BTreeMap<HostKey, Arc<SignInFlow>>> = Mutex::new(BTreeMap::new());

struct FlowInner {
    phase: AntigravitySignInPhase,
    url: Option<String>,
    error: Option<String>,
    stdin: Option<ChildStdin>,
    answered: bool,
    exited: bool,
}

struct SignInFlow {
    id: String,
    intent: AntigravitySignInIntent,
    inner: Mutex<FlowInner>,
    abort: AtomicBool,
    done_cv: Condvar,
}

impl SignInFlow {
    fn snapshot_for(&self, caller_flow_id: Option<&str>) -> Option<AntigravitySignInState> {
        let inner = lock(&self.inner);
        let is_owner = caller_flow_id == Some(self.id.as_str());
        if is_owner {
            Some(AntigravitySignInState {
                flow_id: Some(self.id.clone()),
                phase: inner.phase,
                intent: self.intent,
                url: if inner.phase == AntigravitySignInPhase::Waiting {
                    inner.url.clone()
                } else {
                    None
                },
                error: inner.error.clone(),
            })
        } else if inner.phase.is_running() {
            Some(AntigravitySignInState {
                flow_id: None,
                phase: inner.phase,
                intent: self.intent,
                url: None,
                error: None,
            })
        } else {
            None
        }
    }

    fn is_live(&self) -> bool {
        let inner = lock(&self.inner);
        inner.phase.is_running() || !inner.exited
    }

    fn is_running(&self) -> bool {
        lock(&self.inner).phase.is_running()
    }
}

/// True while an in-app `agy acp --login` flow on this host may hold an unsaved native login.
pub(crate) fn holds_native_login(home: &Path, state_dir: Option<&Path>) -> bool {
    let key = canonical_host_key(home, state_dir);
    let flow = lock(&FLOWS).get(&key).cloned();
    flow.is_some_and(|f| f.is_live())
}

/// Current sign-in status visible to `caller_flow_id`.
pub(crate) fn status_for(
    home: &Path,
    state_dir: Option<&Path>,
    caller_flow_id: Option<&str>,
) -> Option<AntigravitySignInState> {
    let key = canonical_host_key(home, state_dir);
    let flow = lock(&FLOWS).get(&key).cloned()?;
    flow.snapshot_for(caller_flow_id)
}

fn ensure_slot_available(
    flows: &BTreeMap<HostKey, Arc<SignInFlow>>,
    key: &HostKey,
) -> Result<(), String> {
    if let Some(existing) = flows.get(key) {
        if existing.is_running() {
            return Err(OTHER_CLIENT_ERR.to_owned());
        }
        if existing.is_live() {
            return Err(STILL_STOPPING_ERR.to_owned());
        }
    }
    Ok(())
}

fn resolve_host_agy(home: &Path) -> Option<PathBuf> {
    #[cfg(not(test))]
    {
        crate::usage::direct::agy_exe().or_else(|| {
            let local_bin = home.join(".local/bin");
            krill_proto::agent::which("agy", local_bin.as_os_str())
        })
    }
    #[cfg(test)]
    {
        let _ = home;
        None
    }
}

/// Start an execution-host `agy acp --login` flow using the host's installed `agy` binary.
pub(crate) fn start(
    home: &Path,
    state_dir: Option<&Path>,
    intent: AntigravitySignInIntent,
) -> Result<String, String> {
    let exe = resolve_host_agy(home).ok_or_else(|| NOT_INSTALLED_ERR.to_owned())?;
    start_with_exe(home, state_dir, intent, &exe, FLOW_DEADLINE)
}

/// Start an execution-host `agy acp --login` flow with an explicit executable and deadline.
/// Used by both production (`start`) and unit tests with fake `agy` scripts.
pub(crate) fn start_with_exe(
    home: &Path,
    state_dir: Option<&Path>,
    intent: AntigravitySignInIntent,
    exe: &Path,
    deadline_duration: Duration,
) -> Result<String, String> {
    let svc = AntigravityAccountService::for_host(home, state_dir);
    // Wait for any prior account switch/launch restore before modifying or spawning agy.
    svc.settled();

    let key = canonical_host_key(home, state_dir);
    ensure_slot_available(&lock(&FLOWS), &key)?;

    // `agy acp --login` replaces agy's native login, so save any currently signed-in account first
    // regardless of intent (protects against a stale signed-out UI overwriting a terminal login).
    // A locked vault or failed save refuses sign-in rather than lose the existing login.
    let current_state = svc.list_accounts()?;
    if current_state.vault_locked {
        return Err(super::store::LOCKED_RECOVERY_ERR.to_owned());
    }
    if current_state.current_account.is_some() && current_state.active_account_id.is_none() {
        svc.add_current_account()?;
    }

    let flow_id = generate_capability_uuid_v4()?;
    let flow = Arc::new(SignInFlow {
        id: flow_id.clone(),
        intent,
        inner: Mutex::new(FlowInner {
            phase: AntigravitySignInPhase::Starting,
            url: None,
            error: None,
            stdin: None,
            answered: false,
            exited: false,
        }),
        abort: AtomicBool::new(false),
        done_cv: Condvar::new(),
    });

    {
        let mut flows = lock(&FLOWS);
        ensure_slot_available(&flows, &key)?;
        flows.insert(key.clone(), Arc::clone(&flow));
    }

    let mut cmd = krill_proto::os::background_command(exe);
    cmd.args(["acp", "--login"])
        .env("AGY_CLI_DISABLE_AUTO_UPDATE", "true")
        .env("HOME", home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    cmd.env("USERPROFILE", home);
    #[cfg(not(test))]
    {
        use krill_proto::os::ffi::OsStrExt as _;
        let path = std::ffi::OsStr::from_bytes(crate::shell::login_path());
        if !path.is_empty() {
            cmd.env("PATH", path);
        }
    }
    let host_env = current_process_env();
    if !is_real_home(home) {
        // Isolated HOME (tests / sandboxes): write to file token store under `home` and never
        // touch the real OS keyring.
        cmd.env("AGY_CLI_FORCE_FILE_TOKEN_STORAGE", "1");
        cmd.env("SSH_CONNECTION", "krill");
        let mut dir_arg = std::ffi::OsString::from("--gemini_dir=");
        dir_arg.push(home.join(".gemini"));
        cmd.arg(dir_arg);
    } else if !is_antigravity_file_storage_host(&host_env) {
        // Desktop host using OS keyring: clear any accidental file-store overrides so `agy`
        // writes to the same OS keyring that `HostCredentialBackend` reads.
        cmd.env_remove("AGY_CLI_FORCE_FILE_TOKEN_STORAGE");
        cmd.env_remove("SSH_CONNECTION");
    }

    // `crate::child::spawn` owns a Unix process group and Windows Job Object with RAII kill+reap
    // on drop, so spawn/thread-creation failures never leak `agy` or its descendants.
    let mut child = match crate::child::spawn(crate::child::own_group(&mut cmd)) {
        Ok(child) => child,
        Err(err) => {
            clear_spawn_failure(&key, &flow);
            return Err(format!("Could not start agy sign-in: {err}"));
        }
    };

    let stdin = child.stdin.take();
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    {
        let mut inner = lock(&flow.inner);
        inner.stdin = stdin;
    }

    let args = SuperviseFlowArgs {
        flow: Arc::clone(&flow),
        child,
        stdout,
        stderr,
        home: home.to_path_buf(),
        state_dir: state_dir.map(Path::to_path_buf),
        deadline_duration,
    };
    let spawn_result = std::thread::Builder::new()
        .name("agy-sign-in".into())
        .spawn(move || supervise_flow(args));
    if let Err(err) = spawn_result {
        flow.abort.store(true, Ordering::Release);
        clear_spawn_failure(&key, &flow);
        return Err(format!("Could not monitor agy sign-in: {err}"));
    }

    Ok(flow_id)
}

fn clear_spawn_failure(key: &HostKey, flow: &Arc<SignInFlow>) {
    {
        let mut inner = lock(&flow.inner);
        inner.phase = AntigravitySignInPhase::Failed;
        inner.stdin = None;
        inner.exited = true;
    }
    flow.done_cv.notify_all();
    let mut flows = lock(&FLOWS);
    if flows
        .get(key)
        .is_some_and(|existing| Arc::ptr_eq(existing, flow))
    {
        flows.remove(key);
    }
}

/// Deliver the pasted authorization code or callback URL to the running sign-in flow.
pub(crate) fn complete(
    home: &Path,
    state_dir: Option<&Path>,
    flow_id: &str,
    pasted: &str,
) -> Result<(), String> {
    let flow = own_running_flow(home, state_dir, flow_id)?;
    let answer = Zeroizing::new(agy_login_answer(pasted).ok_or_else(|| BAD_ANSWER_ERR.to_owned())?);
    let mut stdin = {
        let mut inner = lock(&flow.inner);
        if !inner.phase.is_running() {
            return Err(NOT_ACTIVE_ERR.to_owned());
        }
        if inner.answered {
            return Err(ALREADY_ANSWERED_ERR.to_owned());
        }
        if inner.phase != AntigravitySignInPhase::Waiting {
            return Err(WAIT_FOR_LINK_ERR.to_owned());
        }
        let Some(stdin) = inner.stdin.take() else {
            return Err(WAIT_FOR_LINK_ERR.to_owned());
        };
        inner.answered = true;
        inner.phase = AntigravitySignInPhase::Verifying;
        stdin
    };
    let line = Zeroizing::new(format!("{}\n", answer.as_str()));
    if stdin.write_all(line.as_bytes()).is_err() || stdin.flush().is_err() {
        // Dropping stdin closes the pipe; supervisor thread reports agy's exit failure.
        return Ok(());
    }
    drop(stdin);
    Ok(())
}

/// Cancel the running sign-in flow, terminate `agy acp --login`, and wait briefly for exit.
pub(crate) fn cancel(home: &Path, state_dir: Option<&Path>, flow_id: &str) -> Result<(), String> {
    let flow = own_running_flow(home, state_dir, flow_id)?;
    // Signal abort BEFORE touching stdin so the supervisor kills the child process group
    // instead of letting stdin EOF advance a shell wrapper into a grandchild process.
    flow.abort.store(true, Ordering::Release);
    {
        let mut inner = lock(&flow.inner);
        if inner.phase.is_running() {
            inner.phase = AntigravitySignInPhase::Cancelled;
            inner.url = None;
            inner.error = None;
        }
    }
    let deadline = Instant::now() + CANCEL_WAIT_TIMEOUT;
    let mut inner = lock(&flow.inner);
    while !inner.exited {
        let rem = deadline.saturating_duration_since(Instant::now());
        if rem.is_zero() {
            break;
        }
        let (next, res) = flow
            .done_cv
            .wait_timeout(inner, rem)
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        inner = next;
        if res.timed_out() {
            break;
        }
    }
    Ok(())
}

fn own_running_flow(
    home: &Path,
    state_dir: Option<&Path>,
    flow_id: &str,
) -> Result<Arc<SignInFlow>, String> {
    let key = canonical_host_key(home, state_dir);
    let flow = lock(&FLOWS)
        .get(&key)
        .cloned()
        .ok_or_else(|| NOT_ACTIVE_ERR.to_owned())?;
    if flow.id != flow_id || !flow.is_running() {
        return Err(NOT_ACTIVE_ERR.to_owned());
    }
    Ok(flow)
}

fn spawn_pipe_reader<R: Read + Send + 'static>(
    name: &str,
    mut pipe: R,
    flow: Arc<SignInFlow>,
    tail: Arc<Mutex<String>>,
) -> Option<std::thread::JoinHandle<()>> {
    std::thread::Builder::new()
        .name(name.into())
        .spawn(move || {
            let mut buf = [0_u8; 4_096];
            let mut pending = String::new();
            loop {
                let n = match pipe.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => n,
                };
                let chunk = String::from_utf8_lossy(buf.get(..n).unwrap_or_default());
                append_tail(&tail, &chunk);
                pending.push_str(&chunk);
                while let Some(pos) = pending.find('\n') {
                    let line = pending.drain(..=pos).collect::<String>();
                    receive_line(&flow, line.trim_end_matches(['\r', '\n']));
                }
                if pending.len() > 32 * 1024 {
                    // Guard against unbounded memory if a child emits gigabytes without newlines.
                    let drop_to = pending.len().saturating_sub(16 * 1024);
                    let boundary = pending.ceil_char_boundary(drop_to);
                    pending.drain(..boundary);
                }
            }
            if !pending.is_empty() {
                receive_line(&flow, pending.trim_end_matches(['\r', '\n']));
            }
        })
        .ok()
}

fn append_tail(tail: &Mutex<String>, chunk: &str) {
    let mut out = lock(tail);
    out.push_str(chunk);
    if out.len() > OUTPUT_TAIL_BYTES {
        let cut = out.len().saturating_sub(OUTPUT_TAIL_BYTES);
        let boundary = out.ceil_char_boundary(cut);
        out.drain(..boundary);
    }
}

fn receive_line(flow: &SignInFlow, line: &str) {
    let Some(link) = agy_login_link(line) else {
        return;
    };
    if flow.abort.load(Ordering::Acquire) {
        return;
    }
    let mut inner = lock(&flow.inner);
    if !inner.phase.is_running() {
        return;
    }
    if let Some(existing) = inner.url.as_deref() {
        if existing != link {
            inner.phase = AntigravitySignInPhase::Failed;
            inner.url = None;
            inner.error = Some(MULTI_LINK_ERR.to_owned());
            // Keep `inner.stdin` open until `supervise_flow` kills the process group so we never
            // deliver a clean stdin EOF ahead of SIGKILL.
            flow.abort.store(true, Ordering::Release);
        }
        return;
    }
    inner.url = Some(link.to_owned());
    inner.phase = AntigravitySignInPhase::Waiting;
}

struct SuperviseFlowArgs {
    flow: Arc<SignInFlow>,
    child: crate::child::Process,
    stdout: Option<std::process::ChildStdout>,
    stderr: Option<std::process::ChildStderr>,
    home: PathBuf,
    state_dir: Option<PathBuf>,
    deadline_duration: Duration,
}

fn supervise_flow(args: SuperviseFlowArgs) {
    enum Outcome {
        Exited(std::process::ExitStatus),
        Aborted,
        Expired,
        WaitFailed,
    }

    let SuperviseFlowArgs {
        flow,
        mut child,
        stdout,
        stderr,
        home,
        state_dir,
        deadline_duration,
    } = args;

    let tail = Arc::new(Mutex::new(String::new()));
    let stdout_handle = stdout.and_then(|p| {
        spawn_pipe_reader("agy-signin-stdout", p, Arc::clone(&flow), Arc::clone(&tail))
    });
    let stderr_handle = stderr.and_then(|p| {
        spawn_pipe_reader("agy-signin-stderr", p, Arc::clone(&flow), Arc::clone(&tail))
    });

    let deadline = Instant::now() + deadline_duration;
    let outcome = loop {
        if flow.abort.load(Ordering::Acquire) {
            break Outcome::Aborted;
        }
        if Instant::now() >= deadline {
            break Outcome::Expired;
        }
        match child.try_wait() {
            Ok(Some(status)) => break Outcome::Exited(status),
            Ok(None) => std::thread::sleep(Duration::from_millis(25)),
            Err(_) => break Outcome::WaitFailed,
        }
    };

    // Always drop the process group / Windows Job Object BEFORE closing stdin and BEFORE joining
    // pipe readers so lingering descendants cannot hold stdout/stderr open and wedge `done_cv`.
    drop(child);
    {
        lock(&flow.inner).stdin = None;
    }
    if let Some(h) = stdout_handle {
        drop(h.join());
    }
    if let Some(h) = stderr_handle {
        drop(h.join());
    }

    match outcome {
        Outcome::Exited(status) if status.success() && !flow.abort.load(Ordering::Acquire) => {
            super::evict_host_agy_caches(&home);
            crate::mobile::evict_agy_models_cache();
            let svc = AntigravityAccountService::for_host(&home, state_dir.as_deref());
            match svc.save_and_select_current_account() {
                Ok(saved) if saved.active_account_id.is_some() => {
                    if let Some(active) = saved.active_account_id.as_deref() {
                        crate::usage::accounts::clear_rate_limited(
                            ProviderKind::Antigravity,
                            active,
                        );
                    }
                    let mut inner = lock(&flow.inner);
                    if inner.phase.is_running() {
                        inner.phase = AntigravitySignInPhase::SignedIn;
                        inner.url = None;
                        inner.error = None;
                    }
                    inner.exited = true;
                }
                Ok(_) => {
                    tracing::warn!(
                        "Antigravity sign-in exited 0 but no native credentials were found to save"
                    );
                    let mut inner = lock(&flow.inner);
                    if inner.phase.is_running() {
                        inner.phase = AntigravitySignInPhase::Failed;
                        inner.url = None;
                        inner.error = Some(
                            "Google sign-in finished, but no Antigravity account was found to save. Refresh and try again."
                                .to_owned(),
                        );
                    }
                    inner.exited = true;
                }
                Err(err) => {
                    tracing::warn!(
                        %err,
                        "Antigravity sign-in succeeded but saving the new account to the vault failed"
                    );
                    let mut inner = lock(&flow.inner);
                    if inner.phase.is_running() {
                        inner.phase = AntigravitySignInPhase::Failed;
                        inner.url = None;
                        inner.error = Some(err);
                    }
                    inner.exited = true;
                }
            }
        }
        Outcome::Expired => {
            let mut inner = lock(&flow.inner);
            if inner.phase.is_running() {
                inner.phase = AntigravitySignInPhase::Expired;
                inner.url = None;
                inner.error = None;
            }
            inner.exited = true;
        }
        Outcome::Aborted => {
            let mut inner = lock(&flow.inner);
            if inner.phase.is_running() {
                inner.phase = AntigravitySignInPhase::Cancelled;
                inner.url = None;
            }
            inner.exited = true;
        }
        Outcome::Exited(_) | Outcome::WaitFailed => {
            let output = lock(&tail).clone();
            let mut inner = lock(&flow.inner);
            if inner.phase.is_running() {
                inner.phase = AntigravitySignInPhase::Failed;
                inner.url = None;
                inner.error = Some(agy_login_failure(&output).to_owned());
            }
            inner.exited = true;
        }
    }
    flow.done_cv.notify_all();
}
