//! Install missing status hooks automatically; only failed setup needs a one-time hint.

use super::{AccessAttempts, Env, FixError, TargetAgent, hook_installed};
use gpui::App;
use krill_proto::hook_install::{Action, CodexTrust, Plan};
use std::ffi::OsStr;
use std::io;
use std::path::Path;

pub(super) static INSTALL_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Background preflight before a local agent is created, so its first launch sees the receipt.
pub(crate) fn ensure_current(id: &str, socket: &Path) {
    if cfg!(test) {
        return;
    }
    let (Some(agent), Some(env)) = (TargetAgent::parse(id), Env::current()) else {
        return;
    };
    if env.is_isolated_state() || !socket.starts_with(&env.state) {
        return;
    }
    if let Err(error) = install(agent, &env, None) {
        tracing::warn!(agent = agent.id(), %error, "automatic status hook setup failed");
    }
}

pub(super) fn install(
    agent: TargetAgent,
    env: &Env,
    search: Option<&OsStr>,
) -> Result<(), FixError> {
    if !crate::settings::agent_status_hooks_enabled_at(&env.state) {
        return Ok(());
    }
    install_enabled(agent, env, search)
}

/// The caller checked the preference, or is applying an explicit toggle to on.
pub(super) fn install_enabled(
    agent: TargetAgent,
    env: &Env,
    search: Option<&OsStr>,
) -> Result<(), FixError> {
    let _guard = INSTALL_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if hook_installed(&env.state, agent.id()) {
        return Ok(());
    }
    let executable = super::stage_helpers(env)?;
    let state = super::canonical_path(&env.state)?;
    let path = super::hook_config_path(agent, env)?;
    let login_path = (search.is_none() && agent.id() == "codex")
        .then(|| super::super::login_path(&env.shell))
        .flatten();
    let plan = Plan::prepare_with_trust(
        Action::Install,
        agent,
        &path,
        &state,
        &executable,
        search.or(login_path.as_deref()),
        CodexTrust::Refresh,
    )
    .map_err(|error| super::hook_prepare_error(agent, &error))?;
    if let Some(plan) = plan {
        super::apply_hook(plan)?;
    }
    Ok(())
}

pub(super) fn note_agent_session_at(agent: TargetAgent, env: Env, cx: &mut App) {
    if !crate::settings::current(cx).agent_status_hooks
        || !cx
            .default_global::<AccessAttempts>()
            .agents
            .insert(agent.id().to_owned())
    {
        return;
    }
    let work = cx.background_executor().spawn(async move {
        match install(agent, &env, None) {
            Err(error) if claim(&env.state, agent) => Some(error.to_string()),
            Ok(()) | Err(_) => None,
        }
    });
    cx.spawn(async move |cx| {
        let error = work.await;
        cx.update(|cx| {
            if !crate::settings::current(cx).agent_status_hooks {
                return;
            }
            let (id, _) = super::hook_needed_content(agent);
            if let Some(error) = error {
                super::publish(&id, error, cx);
            } else {
                super::dismiss_need(&id, cx);
            }
        });
    })
    .detach();
}

/// Runs on the background executor; never prompts users who disabled status hooks.
pub(super) fn claim(state: &Path, agent: TargetAgent) -> bool {
    if !crate::settings::agent_status_hooks_enabled_at(state) || hook_installed(state, agent.id()) {
        return false;
    }
    let result = (|| {
        use krill_proto::os::fs::OpenOptionsExt as _;
        let directory = state.join("onboarding-notices");
        std::fs::create_dir_all(&directory)?;
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(directory.join(format!("hook.{}", agent.id())))
    })();
    match result {
        Ok(_) => true,
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => false,
        Err(error) => {
            tracing::warn!(%error, "hook setup hint could not be recorded");
            false
        }
    }
}
