//! 에이전트 런처(MK-037). 프로필을 holder `Spawn`의 argv·env로 바꾼다.
//!
//! - 에이전트는 로그인·대화형 셸(`-l -i -c`)을 거쳐 띄운다. rc 파일이 만드는 PATH(nvm·mise·`~/.local/bin`)에서
//!   찾아야 Terminal.app에서 친 것과 같은 바이너리가 뜬다.
//!   Windows는 셸 문법을 거치지 않는 내부 런처가 `Command`로 실행한다(npm `.cmd` 포함).
//! - preflight: 셸 안에서 `command -v`로 설치를 먼저 확인하고, 없으면 설치 안내를 pane에 찍고 127로 끝난다.
//! - 환경은 [`Shell`]의 launchd 기준 환경 그대로다(allowlist). krilld의 내부 값이 자식에게 가지 않는다.
//!   프로필의 `env`(OWE-657)는 그 위에 덮어쓴다.
//!   Claude의 세션별 0600 `--settings`만 hook 호출에 `KRILL_STATE_DIR`를 지정한다(사용자 설정 무변경).
//!   모든 세션에는 [`pane_env`]만 새로 넣는다(전역 hook 가드와 `krill hook`의 상태 디렉터리).
//! - Codex는 `krill hook install codex`가 trust까지 끝낸 receipt가 있을 때만 그 고정 명령으로 `-c` hook을 붙인다
//!   (MK-012: trust 해시가 명령 바이트에 묶여 있어 다른 명령은 실행되지 않는다).
//! - `yolo_args`는 [`Permission::Yolo`]일 때만 붙는다([`Profile::argv`]). 배지는 같은 값을 쓰는 UI가 단다.

use krill_proto::agent::hooks::{self, Agent};
use krill_proto::agent::{Permission, Profile};
use krill_proto::os::ffi::OsStrExt as _;
use krill_proto::usage::ProviderKind;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::shell::{Launch, Shell};
use crate::usage::accounts;
use control::LaunchAccount;

pub(crate) mod claude;
mod codex;
pub(crate) mod control;
mod skills;
#[cfg(any(windows, test))]
mod windows;
#[cfg(windows)]
pub use windows::run as run_windows_agent;

/// `$0`=`mk-agent`, `$1`=설치 안내, 나머지=에이전트 argv.
// ponytail: POSIX 문법이라 fish·nu 같은 로그인 셸에서는 깨진다. 지원하려면 그때 `/bin/zsh -l -i -c`로 감싼다.
#[cfg(unix)]
const PREFLIGHT: &str = r#"
case "${KRILL_ACCOUNT_PROVIDER-}" in
  claude) unset CLAUDE_CONFIG_DIR ANTHROPIC_API_KEY ANTHROPIC_AUTH_TOKEN ANTHROPIC_BASE_URL CLAUDE_CODE_USE_BEDROCK CLAUDE_CODE_USE_VERTEX CLAUDE_CODE_USE_FOUNDRY CLAUDECODE CLAUDE_CODE_ENTRYPOINT; if [ -n "${KRILL_ACCOUNT_DIR-}" ]; then export CLAUDE_CONFIG_DIR="$KRILL_ACCOUNT_DIR"; fi;;
  codex) unset CODEX_HOME OPENAI_API_KEY CODEX_API_KEY OPENAI_BASE_URL; if [ -n "${KRILL_ACCOUNT_DIR-}" ]; then export CODEX_HOME="$KRILL_ACCOUNT_DIR"; fi;;
  opencode) unset OPENCODE_CONFIG_DIR OPENCODE_AUTH_CONTENT OPENCODE_DB OPENCODE_API_KEY OPENAI_API_KEY ANTHROPIC_API_KEY GOOGLE_API_KEY GEMINI_API_KEY; if [ -n "${KRILL_ACCOUNT_DIR-}" ]; then export XDG_DATA_HOME="$KRILL_ACCOUNT_DIR/data" XDG_STATE_HOME="$KRILL_ACCOUNT_DIR/state" OPENCODE_DB=opencode.db; fi;;
  agy) if [ -n "${KRILL_ACCOUNT_DIR-}" ]; then export SSH_CONNECTION=krill; fi;;
esac
unset KRILL_ACCOUNT_PROVIDER KRILL_ACCOUNT_DIR
hint=$1; shift; command -v "$1" >/dev/null 2>&1 || { printf '%s\n' "$hint" >&2; exit 127; }; if [ "$(basename "$1")" = agy ] && [ "${2#--gemini_dir=}" != "$2" ]; then SSH_CONNECTION=krill exec "$@"; fi; exec "$@"
"#;

/// 이 프로세스의 환경과 passwd로 에이전트 실행 argv·env를 만든다.
/// 프로그램 파일명이 `claude`면 `UUIDv4`를 사전 발급하고 버전 게이트된 세션별 설정을 주입한다.
/// 파일 준비에 실패하면 holder spawn 전에 오류를 반환한다.
/// 파일 이름은 `<세션 id>-<uuid>.json`이라 세션이 끝나면 호출자가 [`claude::discard`]로 지운다.
pub fn launch(
    profile: &Profile,
    permission: Permission,
    session: krill_proto::SessionId,
) -> io::Result<Launch> {
    launch_in(
        profile,
        permission,
        session,
        krill_proto::daemon::state_dir().as_deref(),
    )
    .map(|(l, _, _)| l)
}

/// [`launch`]와 같되 상태 디렉터리를 지정하고 사전 발급한 대화 ID(Claude `UUIDv4`)와
/// 적용한 계정(OWE-698, 실행 기록용)을 함께 돌려준다.
pub(crate) fn launch_in(
    profile: &Profile,
    permission: Permission,
    session: krill_proto::SessionId,
    state: Option<&Path>,
) -> io::Result<(Launch, Option<String>, Option<LaunchAccount>)> {
    launch_selected_in(profile, permission, session, state, None)
}

const ZSH_ACCOUNT_SYNC_ENV: &str = r#"# Managed by Krill — syncs active AI provider account profiles into login shells.
_krill_self_zdotdir="${ZDOTDIR-}"
if [ -n "${KRILL_ORIG_ZDOTDIR+x}" ]; then
  if [ -n "$KRILL_ORIG_ZDOTDIR" ] && [ "$KRILL_ORIG_ZDOTDIR" != "$_krill_self_zdotdir" ]; then
    export ZDOTDIR="$KRILL_ORIG_ZDOTDIR"
  else
    unset ZDOTDIR
  fi
  unset KRILL_ORIG_ZDOTDIR
else
  unset ZDOTDIR
fi
if [[ -o interactive ]] && ! (( $+functions[compdef] )); then
  typeset -ga __krill_compdef_queue=()
  compdef() {
    builtin emulate -L zsh
    __krill_compdef_queue+=("${(pj:\0:)@}")
  }
fi
if [ "${ZDOTDIR:-$HOME}" != "$_krill_self_zdotdir" ] && [ -f "${ZDOTDIR:-$HOME}/.zshenv" ]; then
  . "${ZDOTDIR:-$HOME}/.zshenv"
fi
unset _krill_self_zdotdir
if [ -z "${KRILL_ACCOUNT_PROVIDER+x}" ]; then
  __krill_sync_accounts() {
    if (( $+functions[__krill_flush_compdef] )); then
      __krill_flush_compdef
    fi
    local _krill_env="${KRILL_STATE_DIR:-$HOME/.config/krill}/active-accounts.env"
    if [ ! -f "$_krill_env" ] && [ -f "$HOME/Library/Application Support/dev.krill.app/active-accounts.env" ]; then
      _krill_env="$HOME/Library/Application Support/dev.krill.app/active-accounts.env"
    fi
    if [ -f "$_krill_env" ]; then
      unset CLAUDE_CONFIG_DIR CODEX_HOME OPENCODE_CONFIG_DIR OPENCODE_DB ANTIGRAVITY_CLI_HOME GEMINI_CLI_HOME
      . "$_krill_env"
      if [ "${CLAUDE_CONFIG_DIR-}" = "$HOME/.claude" ]; then unset CLAUDE_CONFIG_DIR; fi
      if [ "${CODEX_HOME-}" = "$HOME/.codex" ]; then unset CODEX_HOME; fi
      if [ "${ANTIGRAVITY_CLI_HOME-}" = "$HOME" ] || [ "${ANTIGRAVITY_CLI_HOME-}" = "$HOME/.gemini/antigravity-cli" ]; then
        unset ANTIGRAVITY_CLI_HOME
      fi
      if [ -n "${OPENCODE_CONFIG_DIR-}" ] && [ "$OPENCODE_CONFIG_DIR" != "$HOME/.local/share/opencode" ]; then
        export XDG_DATA_HOME="$OPENCODE_CONFIG_DIR/data" XDG_STATE_HOME="$OPENCODE_CONFIG_DIR/state" OPENCODE_DB=opencode.db
        unset OPENCODE_CONFIG_DIR
      else
        unset OPENCODE_CONFIG_DIR OPENCODE_DB
      fi
    fi
  }
  __krill_sync_accounts
  typeset -ga precmd_functions preexec_functions
  if (( ! ${precmd_functions[(I)__krill_sync_accounts]} )); then
    precmd_functions+=(__krill_sync_accounts)
  fi
  if (( ! ${preexec_functions[(I)__krill_sync_accounts]} )); then
    preexec_functions+=(__krill_sync_accounts)
  fi
  agy() {
    __krill_sync_accounts
    local _arg
    for _arg in "$@"; do
      case "$_arg" in
        --gemini_dir|--gemini_dir=*|--gemini-dir|--gemini-dir=*)
          command agy "$@"
          return
          ;;
      esac
    done
    if [ -n "${ANTIGRAVITY_CLI_HOME-}" ] && [ "$ANTIGRAVITY_CLI_HOME" != "$HOME" ] && [ "$ANTIGRAVITY_CLI_HOME" != "$HOME/.gemini/antigravity-cli" ]; then
      SSH_CONNECTION=krill command agy --gemini_dir="$ANTIGRAVITY_CLI_HOME/.gemini" "$@"
    else
      command agy "$@"
    fi
  }
  codex() {
    __krill_sync_accounts
    if [ "${KRILL_CODEX_ISOLATE-1}" = "0" ]; then
      command codex "$@"
      return
    fi
    local _arg
    for _arg in "$@"; do
      case "$_arg" in
        agents|queue|--no-daemon|--remote|--remote=*)
          command codex "$@"
          return
          ;;
      esac
    done
    case "$(command codex --help 2>/dev/null </dev/null)" in
      *--no-daemon*)
        command codex --no-daemon "$@"
        ;;
      *)
        command codex "$@"
        ;;
    esac
  }
fi
if [[ -o interactive ]] && [[ "${functions[compdef]-}" == *__krill_compdef_queue* ]]; then
  __krill_flush_compdef() {
    builtin emulate -L zsh
    typeset -ga precmd_functions
    precmd_functions=(${precmd_functions:#__krill_flush_compdef})
    if (( ${#__krill_compdef_queue} )); then
      if [[ "${functions[compdef]-}" == *__krill_compdef_queue* ]]; then
        unfunction compdef 2>/dev/null
        autoload -Uz compinit && compinit -u
      fi
      if (( $+functions[compdef] )) && [[ "${functions[compdef]-}" != *__krill_compdef_queue* ]]; then
        local _krill_compdef_entry
        for _krill_compdef_entry in "${__krill_compdef_queue[@]}"; do
          compdef "${(@0)_krill_compdef_entry}"
        done
      fi
    elif [[ "${functions[compdef]-}" == *__krill_compdef_queue* ]]; then
      compdef() {
        builtin emulate -L zsh
        unfunction compdef 2>/dev/null
        autoload -Uz compinit && compinit -u
        (( $+functions[compdef] )) && compdef "$@"
      }
    fi
    unset __krill_compdef_queue
    unfunction __krill_flush_compdef 2>/dev/null
  }
  typeset -ga precmd_functions
  if (( ! ${precmd_functions[(I)__krill_flush_compdef]} )); then
    precmd_functions+=(__krill_flush_compdef)
  fi
fi
"#;

/// zsh 세션(`Program::Login` 및 에이전트 `-l -i -c` 런처)에 관리형 `.zshenv`를 주입해
/// 계정 프로필 동기화와 `compinit` 이전 `compdef` 호출 호환을 보장한다.
fn inject_zsh_env(login: &mut Launch, state: &Path) {
    let is_zsh = login
        .argv
        .first()
        .and_then(|a| Path::new(std::ffi::OsStr::from_bytes(a)).file_name())
        .is_some_and(|n| n == "zsh");
    if !is_zsh {
        return;
    }
    let zdotdir = state.join("shell").join("zsh");
    let zshenv = zdotdir.join(".zshenv");
    let ok = {
        use krill_proto::os::fs::DirBuilderExt as _;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&zdotdir)
            .and_then(|()| accounts::write_atomic_0600(&zshenv, ZSH_ACCOUNT_SYNC_ENV.as_bytes()))
            .is_ok()
    };
    if ok {
        let zdotdir_bytes = zdotdir.into_os_string().as_encoded_bytes().to_vec();
        let orig = login
            .env
            .iter()
            .find(|(k, _)| k.as_slice() == b"ZDOTDIR")
            .map(|(_, v)| v.clone())
            .filter(|v| v != &zdotdir_bytes)
            .unwrap_or_default();
        login
            .env
            .retain(|(k, _)| k.as_slice() != b"ZDOTDIR" && k.as_slice() != b"KRILL_ORIG_ZDOTDIR");
        login.env.push((b"KRILL_ORIG_ZDOTDIR".to_vec(), orig));
        login.env.push((b"ZDOTDIR".to_vec(), zdotdir_bytes));
    }
}

/// 로그인 셸(`Program::Login`) 실행 시 활성 계정 프로필 환경 변수와 셸 동기화 훅(`active-accounts.env`)을 주입한다.
pub(crate) fn login_in(state: Option<&Path>) -> Launch {
    let mut login = Shell::Login.launch();
    let Some(state) = state else {
        skills::prepare(None, &login, None);
        return login;
    };
    if cfg!(test)
        && !state.join("accounts.toml").exists()
        && !state.join("active-accounts.env").exists()
    {
        inject_zsh_env(&mut login, state);
        return login;
    }
    let home = accounts::default_home();
    for (prog, provider) in [
        ("claude", ProviderKind::Claude),
        ("codex", ProviderKind::Codex),
        ("opencode", ProviderKind::OpenCode),
        ("agy", ProviderKind::Antigravity),
        ("gemini", ProviderKind::Gemini),
    ] {
        let Some((_, active)) = accounts::active_account_info(&home, Some(state), prog) else {
            continue;
        };
        let dir = &active.profile_dir;
        if dir.is_empty() {
            continue;
        }
        if provider == ProviderKind::Antigravity
            && let Some(host_home) = trust_home_for_runtime()
        {
            sync_agy_profile_hooks_in(&host_home, Path::new(dir)).ok();
        }
        login.env.retain(|(key, _)| {
            !provider
                .credential_overrides()
                .iter()
                .any(|name| key.as_slice() == name.as_bytes())
        });
        if provider == ProviderKind::OpenCode {
            login.env.retain(|(k, _)| {
                !matches!(
                    k.as_slice(),
                    b"OPENCODE_DB" | b"XDG_DATA_HOME" | b"XDG_STATE_HOME" | b"OPENCODE_CONFIG_DIR"
                )
            });
            login
                .env
                .push((b"OPENCODE_DB".to_vec(), b"opencode.db".to_vec()));
            for (key, suffix) in [("XDG_DATA_HOME", "data"), ("XDG_STATE_HOME", "state")] {
                login.env.push((
                    key.as_bytes().to_vec(),
                    Path::new(dir)
                        .join(suffix)
                        .into_os_string()
                        .as_encoded_bytes()
                        .to_vec(),
                ));
            }
        } else {
            let key = provider.env_var().as_bytes();
            login.env.retain(|(k, _)| k.as_slice() != key);
            login.env.push((key.to_vec(), dir.as_bytes().to_vec()));
        }
    }
    inject_zsh_env(&mut login, state);
    skills::prepare(None, &login, None);
    login
}

fn write_zsh_sync_env(state: &Path) -> Option<PathBuf> {
    use krill_proto::os::fs::DirBuilderExt as _;
    let zdotdir = state.join("shell").join("zsh");
    let zshenv = zdotdir.join(".zshenv");
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&zdotdir)
        .and_then(|()| accounts::write_atomic_0600(&zshenv, ZSH_ACCOUNT_SYNC_ENV.as_bytes()))
        .ok()
        .map(|()| zdotdir)
}

/// 기동 시 기존 zsh 동기화 스크립트가 있으면 최신 버전으로 갱신한다.
pub(crate) fn refresh_zsh_sync_env(state: &Path) {
    if state.join("shell").join("zsh").join(".zshenv").exists() {
        write_zsh_sync_env(state);
    }
}

/// OWE-712: 새 세션 하나에만 선택 계정을 적용한다. 전역 활성 계정은 바꾸지 않는다.
pub(crate) fn launch_selected_in(
    profile: &Profile,
    permission: Permission,
    session: krill_proto::SessionId,
    state: Option<&Path>,
    selected: Option<&str>,
) -> io::Result<(Launch, Option<String>, Option<LaunchAccount>)> {
    let selected = selected
        .map(|id| selected_account(profile, state, id))
        .transpose()?;
    let mut login = Shell::Login.launch();
    let account = apply_account_env(&mut login, profile, state, selected.as_ref());
    if let Some(profile) = codex_hooks(profile, state, &login.env)? {
        return Ok((
            wrap_account(&profile, permission, login, account.as_ref()),
            None,
            account,
        ));
    }
    if is(profile, "claude") {
        let state = state.ok_or(io::ErrorKind::NotFound)?;
        let krill = mk_bin()?;
        let version = claude::probe(profile, &login);
        let settings = claude::Settings::create(state, &krill, session, version.as_deref())?;
        let id = settings.id.clone();
        return Ok((
            settings.launch(profile, permission, login)?,
            Some(id),
            account,
        ));
    }
    Ok((
        wrap_account(profile, permission, login, account.as_ref()),
        None,
        account,
    ))
}

/// 재개에 쓸 계정(OWE-699). 요청이 계정을 지정하면(다른 계정으로 이어가기) 그 계정을 검증해 쓰고,
/// 아니면 실행 당시 계정([`resume_account`])이다.
pub(crate) fn resume_target(
    profile: &Profile,
    state: Option<&Path>,
    requested: Option<&str>,
    recorded: Option<&LaunchAccount>,
    session_id: Option<&str>,
) -> io::Result<Option<LaunchAccount>> {
    match requested {
        Some(id) => selected_account(profile, state, id).map(Some),
        None => Ok(resume_account(profile, state, recorded, session_id)),
    }
}

/// 선택한 계정의 프로바이더·실행 경로를 검증한다(Orca 미지원 경로는 거부).
fn selected_account(
    profile: &Profile,
    state: Option<&Path>,
    id: &str,
) -> io::Result<LaunchAccount> {
    let provider = profile
        .program()
        .and_then(|p| Path::new(p).file_name())
        .and_then(|p| p.to_str())
        .and_then(ProviderKind::parse)
        .ok_or(io::ErrorKind::InvalidInput)?;
    let home = accounts::default_home();
    let snap = accounts::refresh_accounts_with(&home, state, false);
    let a = snap
        .accounts
        .iter()
        .find(|a| a.provider == provider && a.id == id)
        .filter(|a| {
            a.host_default || (!a.profile_dir.is_empty() && Path::new(&a.profile_dir).is_dir())
        })
        .ok_or(io::ErrorKind::NotFound)?;
    Ok(LaunchAccount {
        id: a.id.clone(),
        env_key: provider.env_var().into(),
        dir: if a.host_default {
            String::new()
        } else {
            a.profile_dir.clone()
        },
    })
}

/// 이전 에이전트 세션의 대화를 이어받는 실행 argv·env를 만든다(MK-052).
///
/// - Claude: 새 세션별 `--settings`를 만들고 `--resume [<session_id>]`로 이어받는다.
/// - Codex: 완료된 receipt의 `-c` hook과 권한 인자를 유지하고 `resume [<session_id>]`로 이어받는다.
/// - Antigravity(`agy`): `<session_id>`가 있으면 `--conversation <session_id>`, 없으면 `-c`로 이어받는다.
/// - 그 밖의 프로필: 같은 프로필·권한으로 새로 띄운다.
///
/// `account`가 있으면 그 계정 프로필로, 없으면 지금 활성 계정으로 띄운다([`resume_account`], OWE-698).
pub(crate) fn resume_in(
    profile: &Profile,
    permission: Permission,
    session: krill_proto::SessionId,
    state: Option<&Path>,
    session_id: Option<&str>,
    account: Option<&LaunchAccount>,
) -> io::Result<(Launch, Option<String>, Option<LaunchAccount>)> {
    if session_id.is_some_and(|id| !control::valid_session_id(id)) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid agent session id",
        ));
    }
    let mut login = Shell::Login.launch();
    let account = apply_account_env(&mut login, profile, state, account);
    let kept_id = session_id.filter(|s| !s.is_empty()).map(str::to_owned);
    if is(profile, "codex") {
        let hooked = codex_hooks(profile, state, &login.env)?;
        let profile = hooked.as_ref().unwrap_or(profile);
        let mut launch = wrap_account(profile, permission, login, account.as_ref());
        launch.argv.push(b"resume".to_vec());
        if let Some(id) = &kept_id {
            launch.argv.push(id.as_bytes().to_vec());
        }
        return Ok((launch, kept_id, account));
    }
    if is(profile, "claude") {
        let state = state.ok_or(io::ErrorKind::NotFound)?;
        let krill = mk_bin()?;
        let version = claude::probe(profile, &login);
        let settings = if kept_id.is_some() {
            claude::Settings::create(state, &krill, session, version.as_deref())?
        } else {
            claude::Settings::create_for_fork(state, &krill, session, version.as_deref())?
        };
        let launch = settings.resume(profile, permission, login, kept_id.as_deref())?;
        return Ok((launch, kept_id, account));
    }
    if is(profile, "agy") || is(profile, "antigravity") {
        let mut launch = wrap_account(profile, permission, login, account.as_ref());
        if let Some(id) = &kept_id {
            launch.argv.push(b"--conversation".to_vec());
            launch.argv.push(id.as_bytes().to_vec());
        } else {
            launch.argv.push(b"-c".to_vec());
        }
        return Ok((launch, kept_id, account));
    }
    if is(profile, "opencode") {
        let mut launch = wrap_account(profile, permission, login, account.as_ref());
        if let Some(id) = &kept_id {
            launch.argv.push(b"--session".to_vec());
            launch.argv.push(id.as_bytes().to_vec());
        } else {
            launch.argv.push(b"--continue".to_vec());
        }
        return Ok((launch, kept_id, account));
    }
    Ok((
        wrap_account(profile, permission, login, account.as_ref()),
        kept_id,
        account,
    ))
}

/// 실행 중이거나 기록이 남은 에이전트 세션의 대화를 포크해 새 세션으로 띄우는 실행 argv·env를 만든다(OWE-736, MK-182).
///
/// - Claude: 새 세션별 `--settings`를 만들고 `--resume=<session_id> --fork-session`(ID가 없으면 `--continue --fork-session`)으로 포크한다.
/// - Codex: 완료된 receipt의 `-c` hook과 권한 인자를 유지하고 `fork <session_id>`(ID가 없으면 `fork --last`)로 포크한다.
/// - 그 밖의 프로필: 같은 프로필·권한·계정으로 새 세션을 띄운다.
pub(crate) fn fork_in(
    profile: &Profile,
    permission: Permission,
    session: krill_proto::SessionId,
    state: Option<&Path>,
    session_id: Option<&str>,
    account: Option<&LaunchAccount>,
) -> io::Result<(Launch, Option<String>, Option<LaunchAccount>)> {
    if session_id.is_some_and(|id| !control::valid_session_id(id)) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid agent session id",
        ));
    }
    let mut login = Shell::Login.launch();
    let account = apply_account_env(&mut login, profile, state, account);
    let kept_id = session_id.filter(|s| !s.is_empty()).map(str::to_owned);
    if is(profile, "codex") {
        let hooked = codex_hooks(profile, state, &login.env)?;
        let profile = hooked.as_ref().unwrap_or(profile);
        let mut launch = wrap_account(profile, permission, login, account.as_ref());
        launch.argv.push(b"fork".to_vec());
        if let Some(id) = &kept_id {
            launch.argv.push(id.as_bytes().to_vec());
        } else {
            launch.argv.push(b"--last".to_vec());
        }
        return Ok((launch, None, account));
    }
    if is(profile, "claude") {
        let state = state.ok_or(io::ErrorKind::NotFound)?;
        let krill = mk_bin()?;
        let version = claude::probe(profile, &login);
        let settings =
            claude::Settings::create_for_fork(state, &krill, session, version.as_deref())?;
        let launch = settings.fork(profile, permission, login, kept_id.as_deref())?;
        return Ok((launch, None, account));
    }
    Ok((
        wrap_account(profile, permission, login, account.as_ref()),
        None,
        account,
    ))
}

/// 재개·휴면 깨우기에 쓸 계정(OWE-698, OWE-753). 기록된 계정은 프로필 폴더가 남아 있으면 그대로 쓴다(계정을
/// 바꾼 뒤에도 원래 프로필로 이어 간다). 폴더가 없거나 구 기록이면 대화가 실제로 든 프로필(K-13
/// `profile_dir_for_vault_target_in`)을 찾고, 그래도 없으면 `None`(지금 활성 계정)이다.
pub(crate) fn resume_account(
    profile: &Profile,
    state: Option<&Path>,
    recorded: Option<&LaunchAccount>,
    session_id: Option<&str>,
) -> Option<LaunchAccount> {
    resume_account_in(
        &accounts::default_home(),
        profile,
        state,
        recorded,
        session_id,
    )
}

pub(crate) fn resume_account_in(
    home: &Path,
    profile: &Profile,
    state: Option<&Path>,
    recorded: Option<&LaunchAccount>,
    session_id: Option<&str>,
) -> Option<LaunchAccount> {
    use krill_proto::agent_history::{VaultAgent, VaultTarget};
    let program = Path::new(profile.program()?).file_name()?.to_str()?;
    let provider = ProviderKind::parse(program)?;
    let file = accounts::load_file(home, state);
    // 프로필의 프로그램이 바뀌었으면(다른 프로바이더) 기록을 쓰지 않는다.
    if let Some(account) = recorded.filter(|a| {
        (a.env_key == provider.env_var()
            || (provider == ProviderKind::Antigravity && a.env_key.is_empty()))
            && (a.dir.is_empty() || Path::new(&a.dir).is_dir())
    }) {
        let mut account = account.clone();
        if account.id.is_empty() {
            account.id = account_id_in(home, &file, provider, &account.dir);
        }
        return Some(account);
    }
    let agent = match provider {
        ProviderKind::Claude => VaultAgent::Claude,
        ProviderKind::Codex => VaultAgent::Codex,
        ProviderKind::Antigravity => VaultAgent::Agy,
        ProviderKind::OpenCode => VaultAgent::OpenCode,
        ProviderKind::Gemini => return None,
    };
    let id = session_id.filter(|id| control::valid_session_id(id))?;
    let key = provider.env_var();
    let inherited = (provider != ProviderKind::Antigravity)
        .then(|| std::env::var_os(key))
        .flatten()
        .filter(|v| !v.is_empty())
        .map(PathBuf::from);
    let host = inherited
        .clone()
        .unwrap_or_else(|| accounts::host_default_dir(home, provider));
    let target = VaultTarget {
        agent,
        id: id.to_owned(),
        cwd: String::new(),
        modified: 0,
    };
    let dir = crate::agent_history::vault::profile_dir_for_vault_target_in(
        &file,
        &target,
        Some(&host),
        state,
    )?;
    // 기본 프로필이면 새 실행처럼 env를 덮어쓰지 않는다.
    let dir = if inherited.is_none() && dir == host {
        String::new()
    } else {
        dir.into_os_string().into_string().ok()?
    };
    Some(LaunchAccount {
        id: account_id_in(home, &file, provider, &dir),
        env_key: key.to_owned(),
        dir,
    })
}

/// 저장된 실행 기록(`account` → `transcript_path` → `session_id` 볼트 탐색)에서 세션이 속한 계정을 찾는다.
pub(crate) fn account_for_stored_in(
    home: &Path,
    profile: &Profile,
    state: Option<&Path>,
    stored: &control::StoredLaunch,
) -> Option<LaunchAccount> {
    let program = Path::new(profile.program()?).file_name()?.to_str()?;
    let provider = ProviderKind::parse(program)?;
    let file = accounts::load_file(home, state);
    if let Some(account) = stored.account.as_ref().filter(|a| {
        (a.env_key == provider.env_var()
            || (provider == ProviderKind::Antigravity && a.env_key.is_empty()))
            && (a.dir.is_empty() || Path::new(&a.dir).is_dir())
    }) {
        let mut account = account.clone();
        if account.id.is_empty() {
            account.id = account_id_in(home, &file, provider, &account.dir);
        }
        return Some(account);
    }
    if let Some(path) = stored.transcript_path.as_deref().filter(|p| !p.is_empty()) {
        let transcript = Path::new(path);
        let transcript_canon = transcript.canonicalize().ok();
        let sub = match provider {
            ProviderKind::Codex => Some("sessions"),
            ProviderKind::Claude => Some("projects"),
            _ => None,
        };
        for a in &file.accounts {
            if ProviderKind::parse(&a.provider) != Some(provider) {
                continue;
            }
            let cfg = a.config_dir();
            if cfg.as_os_str().is_empty() {
                continue;
            }
            let resolved = if provider == ProviderKind::Codex {
                accounts::resolve_codex_dir(home, &a.id, cfg)
            } else {
                cfg.to_path_buf()
            };
            let canon_match = transcript_canon.as_ref().is_some_and(|tc| {
                resolved.canonicalize().is_ok_and(|rc| tc.starts_with(&rc))
                    || cfg.canonicalize().is_ok_and(|cc| tc.starts_with(&cc))
                    || sub.is_some_and(|s| {
                        resolved
                            .join(s)
                            .canonicalize()
                            .is_ok_and(|sc| tc.starts_with(&sc))
                            || cfg
                                .join(s)
                                .canonicalize()
                                .is_ok_and(|sc| tc.starts_with(&sc))
                    })
            });
            if transcript.starts_with(&resolved) || transcript.starts_with(cfg) || canon_match {
                return Some(LaunchAccount {
                    id: a.id.clone(),
                    env_key: provider.env_var().to_owned(),
                    dir: resolved.to_string_lossy().into_owned(),
                });
            }
        }
        let host = accounts::host_default_dir(home, provider);
        let host_canon_match = transcript_canon.as_ref().is_some_and(|tc| {
            host.canonicalize().is_ok_and(|hc| tc.starts_with(&hc))
                || sub.is_some_and(|s| {
                    host.join(s)
                        .canonicalize()
                        .is_ok_and(|sc| tc.starts_with(&sc))
                })
        });
        if !host.as_os_str().is_empty() && (transcript.starts_with(&host) || host_canon_match) {
            return Some(LaunchAccount {
                id: accounts::host_default_id(provider),
                env_key: provider.env_var().to_owned(),
                dir: String::new(),
            });
        }
    }
    resume_account_in(
        home,
        profile,
        state,
        stored.account.as_ref(),
        stored.session_id.as_deref(),
    )
}

/// 지금 활성 계정. 호스트 기본이면 `dir`이 빈 기록이라 이후 계정을 바꿔도 재개는 기본 프로필로 간다.
fn active_account(state: Option<&Path>, program: &str) -> Option<LaunchAccount> {
    let provider = ProviderKind::parse(Path::new(program).file_name()?.to_str()?)?;
    let home = accounts::default_home();
    let (id, dir) = match accounts::active_account_info(&home, state, program) {
        Some((_, active)) => (active.id, active.profile_dir),
        None => (accounts::host_default_id(provider), String::new()),
    };
    Some(LaunchAccount {
        id,
        env_key: provider.env_var().to_owned(),
        dir,
    })
}

/// `accounts.toml`에서 프로필 폴더로 계정 id를 찾는다. 기본 프로필은 호스트 id, 없으면 빈 값.
fn account_id_in(
    home: &Path,
    file: &accounts::AccountsFile,
    provider: ProviderKind,
    dir: &str,
) -> String {
    if dir.is_empty() {
        return accounts::host_default_id(provider);
    }
    file.accounts
        .iter()
        .find(|a| {
            if ProviderKind::parse(&a.provider) != Some(provider) {
                return false;
            }
            let cfg = a.config_dir();
            if cfg == Path::new(dir) {
                return true;
            }
            provider == ProviderKind::Codex
                && accounts::resolve_codex_dir(home, &a.id, cfg) == Path::new(dir)
        })
        .map(|a| a.id.clone())
        .unwrap_or_default()
}

/// 워크스페이스 신뢰 프리셋이 상태 디렉터리에서 명시적으로 활성화됐는지 확인한다(기본값 `false`, OWE-737).
pub(crate) fn is_trust_preset_enabled_in(state_dir: Option<&Path>) -> bool {
    krill_proto::git::is_workspace_trust_preset_enabled(state_dir)
}

/// 에이전트 실행 전에 작업 디렉터리를 각 CLI의 로컬 trust 설정에 사전 등록해 첫 실행 trust
/// 대화상자로 워커가 멈추지 않게 한다. Antigravity CLI(`agy`)는 항상 선택 계정 프로필 home의
/// `~/.gemini/antigravity-cli/settings.json` `trustedWorkspaces`에 등록하고(OWE-706),
/// Claude·Codex·Cursor는 사용자 동의(설정 토글 `workspace-trust-preset.enabled`, 기본 꺼짐)가
/// 켜져 있을 때만 등록한다(OWE-737).
///
/// 참고: `.ref/orca/src/main/providers/claude.ts`, `codex.ts`, `cursor.ts`
/// (Copyright (c) 2026 Lovecast Inc., MIT License, `.ref/orca/LICENSE`).
pub(crate) fn pre_trust_workspace(profile: &Profile, cwd: &[u8], account: Option<&LaunchAccount>) {
    let agy = is(profile, "agy") || is(profile, "antigravity");
    let claude = is(profile, "claude");
    let codex = is(profile, "codex");
    let cursor = is(profile, "cursor") || is(profile, "cursor-agent");
    if !agy && !claude && !codex && !cursor {
        return;
    }
    if !agy && !is_trust_preset_enabled_in(krill_proto::daemon::state_dir().as_deref()) {
        return;
    }
    let account_dir = account
        .filter(|account| !account.dir.is_empty())
        .map(|account| PathBuf::from(&account.dir));
    let path = if cwd.is_empty() {
        match std::env::current_dir() {
            Ok(p) => p,
            Err(_) => return,
        }
    } else {
        PathBuf::from(std::ffi::OsStr::from_bytes(cwd))
    };
    if agy {
        if let (Some(host_home), Some(acc_home)) =
            (trust_home_for_runtime(), account_dir.as_deref())
        {
            sync_agy_profile_hooks_in(&host_home, acc_home).ok();
        }
        // 계정 프로필이 있으면 그 폴더가 agy의 home이다(OWE-706).
        let Some(home) = account_dir.or_else(trust_home_for_runtime) else {
            return;
        };
        pre_trust_workspace_in(&home, &path).ok();
        return;
    }
    let Some(home) = trust_home_for_runtime() else {
        return;
    };
    // 계정 프로필 폴더(CLAUDE_CONFIG_DIR·CODEX_HOME) → 프로필 env → 프로세스 env 순.
    let config_dir = |key: &str| {
        account_dir
            .clone()
            .or_else(|| {
                profile
                    .env
                    .get(key)
                    .filter(|v| !v.is_empty())
                    .map(PathBuf::from)
            })
            .or_else(|| {
                std::env::var_os(key)
                    .filter(|v| !v.is_empty())
                    .map(PathBuf::from)
            })
    };
    if claude {
        pre_trust_claude_in(&home, config_dir("CLAUDE_CONFIG_DIR").as_deref(), &path).ok();
    } else if codex {
        pre_trust_codex_in(&home, config_dir("CODEX_HOME").as_deref(), &path).ok();
    } else {
        pre_trust_cursor_in(&home, &path).ok();
    }
}

/// 새 워크트리 생성 시 동의 플래그가 켜져 있으면 Claude·Codex·Cursor·Antigravity 모두에 신뢰를 등록한다(OWE-737).
pub(crate) fn pre_trust_all_agents(cwd: &Path) {
    let state_dir = krill_proto::daemon::state_dir();
    let Some(home) = trust_home_for_runtime() else {
        return;
    };
    let claude_dir = std::env::var_os("CLAUDE_CONFIG_DIR")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from);
    let codex_dir = std::env::var_os("CODEX_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from);
    pre_trust_all_agents_if_enabled(
        state_dir.as_deref(),
        &home,
        claude_dir.as_deref(),
        codex_dir.as_deref(),
        cwd,
    );
}

/// 상태 디렉터리의 `workspace-trust-preset.enabled` 플래그가 켜져 있을 때만 신뢰 프리셋을 기록한다(OWE-737).
pub(crate) fn pre_trust_all_agents_if_enabled(
    state_dir: Option<&Path>,
    home: &Path,
    claude_config_dir: Option<&Path>,
    codex_home: Option<&Path>,
    cwd: &Path,
) -> bool {
    if !is_trust_preset_enabled_in(state_dir) {
        return false;
    }
    pre_trust_all_agents_in(home, claude_config_dir, codex_home, cwd);
    true
}

/// 테스트·내부 호출에서 명시적 `home`·프로필 경로로 모든 에이전트의 워크스페이스 신뢰를 등록한다(OWE-737).
pub(crate) fn pre_trust_all_agents_in(
    home: &Path,
    claude_config_dir: Option<&Path>,
    codex_home: Option<&Path>,
    cwd: &Path,
) {
    pre_trust_claude_in(home, claude_config_dir, cwd).ok();
    pre_trust_codex_in(home, codex_home, cwd).ok();
    pre_trust_cursor_in(home, cwd).ok();
    pre_trust_workspace_in(home, cwd).ok();
}

fn trust_home_for_runtime() -> Option<PathBuf> {
    if cfg!(test) {
        return None;
    }
    let home = crate::usage::accounts::default_home();
    if std::env::var_os("KRILL_TEST_OFFLINE").is_some_and(|v| !v.is_empty() && v != "0") {
        let tmp = std::env::temp_dir();
        let tmp_canon = std::fs::canonicalize(&tmp).unwrap_or_else(|_| tmp.clone());
        let home_canon = std::fs::canonicalize(&home).unwrap_or_else(|_| home.clone());
        if !home.starts_with(&tmp)
            && !home_canon.starts_with(&tmp_canon)
            && !home.starts_with("/tmp")
            && !home.starts_with("/private/tmp")
            && !home.starts_with("/var/folders")
            && !home.starts_with("/private/var/folders")
        {
            return None;
        }
    }
    Some(home)
}

/// 루트(`/`)나 홈 디렉터리 자체는 신뢰 범위를 넓히지 않도록 거부한다.
pub(crate) fn is_too_broad_to_pre_trust(home: &Path, cwd: &Path) -> bool {
    if cwd.as_os_str().is_empty() || cwd == Path::new("/") || cwd == home {
        return true;
    }
    let cwd_canon = std::fs::canonicalize(cwd).unwrap_or_else(|_| cwd.to_path_buf());
    let home_canon = std::fs::canonicalize(home).unwrap_or_else(|_| home.to_path_buf());
    cwd_canon == Path::new("/") || (!home.as_os_str().is_empty() && cwd_canon == home_canon)
}

fn trust_candidates(cwd: &Path) -> Vec<String> {
    let mut candidates = Vec::new();
    if let Some(s) = cwd.to_str().filter(|s| !s.is_empty()) {
        candidates.push(s.to_owned());
    }
    if let Ok(canon) = std::fs::canonicalize(cwd)
        && let Some(s) = canon.to_str().filter(|s| !s.is_empty())
        && !candidates.iter().any(|c| c == s)
    {
        candidates.push(s.to_owned());
    }
    candidates
}

/// K-09: 설정 파일 읽기-수정-쓰기를 프로세스 전역에서 직렬화한다.
/// 병렬 워커 기동 때 trust 추가분이 유실되던 결함을 막는다. 파일 IO만 안에서 해 리프 잠금이다.
static TRUST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
static TRUST_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn read_trust_file_snapshot(target: &Path) -> io::Result<Option<Vec<u8>>> {
    match std::fs::symlink_metadata(target) {
        Ok(meta) if meta.file_type().is_symlink() => {
            tracing::warn!(path = %target.display(), "refusing to read symlink trust config");
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "symlink trust config refused",
            ));
        }
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    }
    match std::fs::read(target) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// 임시 파일에 먼저 기록한 뒤 `rename` 직전 원본 바이트(CAS)를 다시 대조해,
/// 외부 프로세스(실행 중인 Claude Code·Codex 등)가 동시에 수정했으면 덮어쓰지 않고 건너뛴다(OWE-737).
pub(crate) fn write_trust_file_cas(
    target: &Path,
    expected_original: Option<&[u8]>,
    bytes: &[u8],
    before_rename: Option<&dyn Fn(&Path)>,
) -> io::Result<bool> {
    use krill_proto::os::fs::{DirBuilderExt as _, OpenOptionsExt as _};
    use std::fs::{DirBuilder, OpenOptions};
    use std::io::Write as _;
    use std::sync::atomic::Ordering;

    if let Some(dir) = target.parent() {
        DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
    }
    if std::fs::symlink_metadata(target).is_ok_and(|m| m.file_type().is_symlink()) {
        tracing::warn!(path = %target.display(), "refusing to write symlink trust config");
        return Ok(false);
    }
    #[cfg(unix)]
    let mode = std::fs::metadata(target).map_or(0o600, |m| {
        std::os::unix::fs::PermissionsExt::mode(&m.permissions()) & 0o777
    });
    #[cfg(windows)]
    let mode = 0o600;
    let seq = TRUST_SEQ.fetch_add(1, Ordering::Relaxed);
    let tmp = target.with_extension(format!("tmp.{}.{}", std::process::id(), seq));
    let write_res = (|| -> io::Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(mode)
            .open(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all().ok();
        Ok(())
    })();
    if let Err(e) = write_res {
        std::fs::remove_file(&tmp).ok();
        return Err(e);
    }
    if let Some(hook) = before_rename {
        hook(target);
    }
    if std::fs::symlink_metadata(target).is_ok_and(|m| m.file_type().is_symlink()) {
        std::fs::remove_file(&tmp).ok();
        tracing::warn!(path = %target.display(), "trust config became symlink before rename; skipping");
        return Ok(false);
    }
    let unchanged = match (expected_original, std::fs::read(target)) {
        (None, Err(e)) if e.kind() == io::ErrorKind::NotFound => true,
        (Some(orig), Ok(cur)) => cur.as_slice() == orig,
        _ => false,
    };
    if !unchanged {
        std::fs::remove_file(&tmp).ok();
        tracing::warn!(
            path = %target.display(),
            "trust config modified concurrently by another process; skipping write"
        );
        return Ok(false);
    }
    if let Err(e) = std::fs::rename(&tmp, target) {
        std::fs::remove_file(&tmp).ok();
        return Err(e);
    }
    Ok(true)
}

#[derive(Debug, Clone)]
struct JsonMemberSpan {
    key: String,
    val_start: usize,
    val_end: usize,
}

#[derive(Debug, Clone)]
struct JsonObjectSpan {
    open: usize,
    close: usize,
    members: Vec<JsonMemberSpan>,
}

fn skip_json_ws(b: &[u8], mut i: usize) -> usize {
    while let Some(&ch) = b.get(i) {
        if matches!(ch, b' ' | b'\t' | b'\r' | b'\n') {
            i += 1;
        } else {
            break;
        }
    }
    i
}

fn scan_json_string(s: &str, start: usize) -> Option<(String, usize)> {
    let b = s.as_bytes();
    if b.get(start).copied() != Some(b'"') {
        return None;
    }
    let mut i = start + 1;
    while let Some(&ch) = b.get(i) {
        if ch == b'\\' {
            i += 2;
        } else if ch == b'"' {
            let raw = s.get(start..=i)?;
            let decoded: String = serde_json::from_str(raw).ok()?;
            return Some((decoded, i + 1));
        } else {
            i += 1;
        }
    }
    None
}

fn scan_json_value_end(s: &str, start: usize) -> Option<usize> {
    let b = s.as_bytes();
    let i = skip_json_ws(b, start);
    match b.get(i).copied()? {
        b'"' => scan_json_string(s, i).map(|(_, end)| end),
        b'{' => scan_json_object(s, i).map(|obj| obj.close + 1),
        b'[' => scan_json_array_end(s, i),
        _ => {
            let mut j = i;
            while let Some(&ch) = b.get(j) {
                if matches!(ch, b' ' | b'\t' | b'\r' | b'\n' | b',' | b'}' | b']') {
                    break;
                }
                j += 1;
            }
            (j > i).then_some(j)
        }
    }
}

fn scan_json_array_end(s: &str, start: usize) -> Option<usize> {
    let b = s.as_bytes();
    if b.get(start).copied() != Some(b'[') {
        return None;
    }
    let mut i = skip_json_ws(b, start + 1);
    if b.get(i).copied() == Some(b']') {
        return Some(i + 1);
    }
    loop {
        i = scan_json_value_end(s, i)?;
        i = skip_json_ws(b, i);
        match b.get(i).copied()? {
            b',' => i = skip_json_ws(b, i + 1),
            b']' => return Some(i + 1),
            _ => return None,
        }
    }
}

fn scan_json_object(s: &str, start: usize) -> Option<JsonObjectSpan> {
    let b = s.as_bytes();
    let open = skip_json_ws(b, start);
    if b.get(open).copied() != Some(b'{') {
        return None;
    }
    let mut i = skip_json_ws(b, open + 1);
    let mut members = Vec::new();
    if b.get(i).copied() == Some(b'}') {
        return Some(JsonObjectSpan {
            open,
            close: i,
            members,
        });
    }
    loop {
        let (key, after_key) = scan_json_string(s, i)?;
        i = skip_json_ws(b, after_key);
        if b.get(i).copied() != Some(b':') {
            return None;
        }
        let val_start = skip_json_ws(b, i + 1);
        let val_end = scan_json_value_end(s, val_start)?;
        members.push(JsonMemberSpan {
            key,
            val_start,
            val_end,
        });
        i = skip_json_ws(b, val_end);
        match b.get(i).copied()? {
            b',' => i = skip_json_ws(b, i + 1),
            b'}' => {
                return Some(JsonObjectSpan {
                    open,
                    close: i,
                    members,
                });
            }
            _ => return None,
        }
    }
}

fn upsert_json_object_member(
    s: &str,
    obj: &JsonObjectSpan,
    key: &str,
    raw_value_json: &str,
    indent: &str,
) -> Option<String> {
    if let Some(existing) = obj.members.iter().rfind(|m| m.key == key) {
        let prefix = s.get(..existing.val_start)?;
        let suffix = s.get(existing.val_end..)?;
        return Some(format!("{prefix}{raw_value_json}{suffix}"));
    }
    let key_json = serde_json::to_string(key).ok()?;
    let multiline = s
        .get(obj.open..=obj.close)
        .is_some_and(|slice| slice.contains('\n'));
    if let Some(last) = obj.members.last() {
        let prefix = s.get(..last.val_end)?;
        let suffix = s.get(last.val_end..)?;
        if multiline {
            Some(format!(
                "{prefix},\n{indent}{key_json}: {raw_value_json}{suffix}"
            ))
        } else {
            Some(format!("{prefix}, {key_json}: {raw_value_json}{suffix}"))
        }
    } else {
        let prefix = s.get(..obj.open)?;
        let suffix = s.get((obj.close + 1)..)?;
        let parent_indent = indent.strip_suffix("  ").unwrap_or("");
        Some(format!(
            "{prefix}{{\n{indent}{key_json}: {raw_value_json}\n{parent_indent}}}{suffix}"
        ))
    }
}

fn patch_claude_trust_json(
    existing: Option<&str>,
    candidates: &[String],
) -> Option<(String, bool)> {
    let Some(text) = existing else {
        let mut entries = Vec::with_capacity(candidates.len());
        for cand in candidates {
            let key_json = serde_json::to_string(cand).ok()?;
            entries.push(format!(
                "    {key_json}: {{\n      \"hasTrustDialogAccepted\": true\n    }}"
            ));
        }
        let out = format!(
            "{{\n  \"projects\": {{\n{}\n  }}\n}}\n",
            entries.join(",\n")
        );
        serde_json::from_str::<serde_json::Value>(&out).ok()?;
        return Some((out, true));
    };

    let val: serde_json::Value = serde_json::from_str(text).ok()?;
    let root_map = val.as_object()?;
    if let Some(projects_val) = root_map.get("projects") {
        let projects_map = projects_val.as_object()?;
        for cand in candidates {
            if let Some(entry_val) = projects_map.get(cand)
                && !entry_val.is_object()
            {
                return None;
            }
        }
    }

    let mut cur = text.to_owned();
    let mut changed = false;
    for cand in candidates {
        let root_span = scan_json_object(&cur, 0)?;
        if let Some(proj_member) = root_span.members.iter().rfind(|m| m.key == "projects") {
            let projects_span = scan_json_object(&cur, proj_member.val_start)?;
            if let Some(cand_member) = projects_span.members.iter().rfind(|m| m.key == *cand) {
                let cand_span = scan_json_object(&cur, cand_member.val_start)?;
                let already_trusted = cand_span
                    .members
                    .iter()
                    .rfind(|m| m.key == "hasTrustDialogAccepted")
                    .and_then(|m| cur.get(m.val_start..m.val_end))
                    .is_some_and(|raw| raw.trim() == "true");
                if !already_trusted {
                    cur = upsert_json_object_member(
                        &cur,
                        &cand_span,
                        "hasTrustDialogAccepted",
                        "true",
                        "      ",
                    )?;
                    changed = true;
                }
            } else {
                let entry_raw = "{\n      \"hasTrustDialogAccepted\": true\n    }";
                cur = upsert_json_object_member(&cur, &projects_span, cand, entry_raw, "    ")?;
                changed = true;
            }
        } else {
            let cand_key = serde_json::to_string(cand).ok()?;
            let projects_raw = format!(
                "{{\n    {cand_key}: {{\n      \"hasTrustDialogAccepted\": true\n    }}\n  }}"
            );
            cur = upsert_json_object_member(&cur, &root_span, "projects", &projects_raw, "  ")?;
            changed = true;
        }
    }

    if !changed {
        return Some((cur, false));
    }
    if !cur.ends_with('\n') {
        cur.push('\n');
    }
    serde_json::from_str::<serde_json::Value>(&cur).ok()?;
    Some((cur, true))
}

fn patch_antigravity_settings_json(
    existing: Option<&str>,
    candidates: &[String],
) -> Option<(String, bool)> {
    let Some(text) = existing else {
        let mut elems = Vec::with_capacity(candidates.len());
        for cand in candidates {
            elems.push(format!("    {}", serde_json::to_string(cand).ok()?));
        }
        let out = format!(
            "{{\n  \"trustedWorkspaces\": [\n{}\n  ]\n}}\n",
            elems.join(",\n")
        );
        serde_json::from_str::<serde_json::Value>(&out).ok()?;
        return Some((out, true));
    };

    let val: serde_json::Value = serde_json::from_str(text).ok()?;
    let root_map = val.as_object()?;
    let root_span = scan_json_object(text, 0)?;

    let mut out = if let Some(tw_val) = root_map.get("trustedWorkspaces") {
        let arr = tw_val.as_array()?;
        let mut missing = Vec::new();
        for cand in candidates {
            if !arr.iter().any(|v| v.as_str() == Some(cand.as_str())) && !missing.contains(cand) {
                missing.push(cand.clone());
            }
        }
        if missing.is_empty() {
            return Some((text.to_owned(), false));
        }
        let tw_member = root_span
            .members
            .iter()
            .rfind(|m| m.key == "trustedWorkspaces")?;
        let arr_raw = text.get(tw_member.val_start..tw_member.val_end)?;
        let close_rel = arr_raw.rfind(']')?;
        let before_close = arr_raw
            .get(..close_rel)?
            .trim_end_matches([' ', '\t', '\r', '\n']);
        let mut encoded = Vec::with_capacity(missing.len());
        for m in &missing {
            encoded.push(serde_json::to_string(m).ok()?);
        }
        let new_arr_raw = if arr.is_empty() {
            format!("[\n    {}\n  ]", encoded.join(",\n    "))
        } else if arr_raw.contains('\n') {
            let tail = arr_raw.get(before_close.len()..)?;
            format!("{before_close},\n    {}{tail}", encoded.join(",\n    "))
        } else {
            let tail = arr_raw.get(before_close.len()..)?;
            format!("{before_close}, {}{tail}", encoded.join(", "))
        };
        let prefix = text.get(..tw_member.val_start)?;
        let suffix = text.get(tw_member.val_end..)?;
        format!("{prefix}{new_arr_raw}{suffix}")
    } else {
        let mut encoded = Vec::with_capacity(candidates.len());
        for cand in candidates {
            encoded.push(serde_json::to_string(cand).ok()?);
        }
        let arr_raw = format!("[\n    {}\n  ]", encoded.join(",\n    "));
        upsert_json_object_member(text, &root_span, "trustedWorkspaces", &arr_raw, "  ")?
    };

    if !out.ends_with('\n') {
        out.push('\n');
    }
    serde_json::from_str::<serde_json::Value>(&out).ok()?;
    Some((out, true))
}

/// Claude Code 로컬 설정(`<CLAUDE_CONFIG_DIR>/.claude.json` 또는 `~/.claude.json`)의
/// `projects[path].hasTrustDialogAccepted = true`를 설정한다(OWE-737).
/// 기존 JSON의 키 순서·알 수 없는 필드를 보존하며, 손상됐거나 예상 밖 타입이면 건드리지 않고 `Ok(false)`를 돌려준다.
pub(crate) fn pre_trust_claude_in(
    home: &Path,
    claude_config_dir: Option<&Path>,
    cwd: &Path,
) -> io::Result<bool> {
    pre_trust_claude_in_with_hook(home, claude_config_dir, cwd, None)
}

pub(crate) fn pre_trust_claude_in_with_hook(
    home: &Path,
    claude_config_dir: Option<&Path>,
    cwd: &Path,
    before_rename: Option<&dyn Fn(&Path)>,
) -> io::Result<bool> {
    if is_too_broad_to_pre_trust(home, cwd) {
        return Ok(false);
    }
    let candidates = trust_candidates(cwd);
    if candidates.is_empty() {
        return Ok(false);
    }
    let _guard = crate::session::lock(&TRUST_LOCK);
    let config_path = if let Some(dir) = claude_config_dir.filter(|d| !d.as_os_str().is_empty()) {
        dir.join(".claude.json")
    } else {
        let primary = home.join(".claude.json");
        let legacy = home.join(".claude/.config.json");
        if primary.exists() || !legacy.exists() {
            primary
        } else {
            legacy
        }
    };
    let Ok(orig_bytes) = read_trust_file_snapshot(&config_path) else {
        return Ok(false);
    };
    let existing_str = match orig_bytes.as_deref() {
        Some(b) => {
            if let Ok(s) = std::str::from_utf8(b) {
                Some(s)
            } else {
                tracing::warn!(path = %config_path.display(), "claude config is not valid UTF-8; skipping");
                return Ok(false);
            }
        }
        None => None,
    };
    let Some((next, changed)) = patch_claude_trust_json(existing_str, &candidates) else {
        tracing::warn!(path = %config_path.display(), "skipping claude trust preset due to malformed JSON or unexpected schema");
        return Ok(false);
    };
    if !changed {
        return Ok(false);
    }
    write_trust_file_cas(
        &config_path,
        orig_bytes.as_deref(),
        next.as_bytes(),
        before_rename,
    )
}

/// Codex CLI 설정(`<CODEX_HOME>/config.toml` 또는 `~/.codex/config.toml`)에
/// `[projects."<canonical_path>"] trust_level = "trusted"`를 등록한다(OWE-737).
pub(crate) fn pre_trust_codex_in(
    home: &Path,
    codex_home: Option<&Path>,
    cwd: &Path,
) -> io::Result<bool> {
    pre_trust_codex_in_with_hook(home, codex_home, cwd, None)
}

pub(crate) fn pre_trust_codex_in_with_hook(
    home: &Path,
    codex_home: Option<&Path>,
    cwd: &Path,
    before_rename: Option<&dyn Fn(&Path)>,
) -> io::Result<bool> {
    if is_too_broad_to_pre_trust(home, cwd) {
        return Ok(false);
    }
    let canon = std::fs::canonicalize(cwd).unwrap_or_else(|_| cwd.to_path_buf());
    let Some(project_path) = canon.to_str().filter(|s| !s.is_empty()) else {
        return Ok(false);
    };
    let _guard = crate::session::lock(&TRUST_LOCK);
    let config_path = if let Some(dir) = codex_home.filter(|d| !d.as_os_str().is_empty()) {
        dir.join("config.toml")
    } else {
        home.join(".codex/config.toml")
    };
    let Ok(orig_bytes) = read_trust_file_snapshot(&config_path) else {
        return Ok(false);
    };
    let existing = match orig_bytes.as_deref() {
        Some(b) => {
            if let Ok(s) = std::str::from_utf8(b) {
                s
            } else {
                tracing::warn!(path = %config_path.display(), "codex config.toml is not valid UTF-8; skipping");
                return Ok(false);
            }
        }
        None => "",
    };
    let Some((next, changed)) = upsert_codex_project_trust_checked(existing, project_path) else {
        tracing::warn!(path = %config_path.display(), "skipping codex trust preset due to malformed TOML or unexpected schema");
        return Ok(false);
    };
    if !changed {
        return Ok(false);
    }
    write_trust_file_cas(
        &config_path,
        orig_bytes.as_deref(),
        next.as_bytes(),
        before_rename,
    )
}

pub(crate) fn upsert_codex_project_trust_checked(
    existing: &str,
    project_path: &str,
) -> Option<(String, bool)> {
    if !existing.trim().is_empty() {
        let parsed: toml::Value = toml::from_str(existing).ok()?;
        let root_tbl = parsed.as_table()?;
        if let Some(projects_val) = root_tbl.get("projects") {
            let projects_tbl = projects_val.as_table()?;
            if let Some(proj_val) = projects_tbl.get(project_path)
                && !proj_val.is_table()
            {
                return None;
            }
        }
    }
    let (next, changed) = upsert_codex_project_trust(existing, project_path);
    if changed {
        toml::from_str::<toml::Value>(&next).ok()?;
    }
    Some((next, changed))
}

pub(crate) fn upsert_codex_project_trust(existing: &str, project_path: &str) -> (String, bool) {
    let escaped = project_path.replace('\\', "\\\\").replace('"', "\\\"");
    let header_dq = format!("projects.\"{escaped}\"");
    let header_sq = format!("projects.'{project_path}'");
    let target_header = format!("[{header_dq}]");
    let trust_line = "trust_level = \"trusted\"";

    let mut lines: Vec<String> = if existing.is_empty() {
        Vec::new()
    } else {
        existing.lines().map(str::to_owned).collect()
    };

    let section_idx = lines.iter().position(|line| {
        let trimmed = line.trim();
        let Some(inner) = trimmed.strip_prefix('[').and_then(|s| s.strip_suffix(']')) else {
            return false;
        };
        let inner = inner.trim();
        inner == header_dq || inner == header_sq
    });

    if let Some(start) = section_idx {
        let mut end = lines.len();
        for (i, line) in lines.iter().enumerate().skip(start + 1) {
            let trimmed = line.trim();
            if trimmed.starts_with('[') && trimmed.ends_with(']') {
                end = i;
                break;
            }
        }
        if let Some(slice) = lines.get_mut((start + 1)..end) {
            for line in slice {
                let without_comment = line.split('#').next().unwrap_or("").trim();
                if let Some((key, val)) = without_comment.split_once('=')
                    && key.trim() == "trust_level"
                {
                    let val = val.trim();
                    if val == "\"trusted\"" || val == "'trusted'" {
                        return (existing.to_owned(), false);
                    }
                    trust_line.clone_into(line);
                    let mut out = lines.join("\n");
                    out.push('\n');
                    return (out, true);
                }
            }
        }
        let mut insert_at = start + 1;
        for (i, line) in lines.iter().enumerate().take(end).skip(start + 1) {
            if !line.trim().is_empty() {
                insert_at = i + 1;
            }
        }
        lines.insert(insert_at, trust_line.to_owned());
        let mut out = lines.join("\n");
        out.push('\n');
        return (out, true);
    }

    let trimmed = existing.trim_end_matches(['\r', '\n']);
    let next = if trimmed.is_empty() {
        format!("{target_header}\n{trust_line}\n")
    } else {
        format!("{trimmed}\n\n{target_header}\n{trust_line}\n")
    };
    (next, true)
}

/// Cursor 프로젝트 슬러그: 선행 `/`를 떼고 `/` 및 파일시스템 예약 문자를 `-`로 바꾼다.
pub(crate) fn cursor_project_slug(canonical_path: &str) -> String {
    canonical_path
        .trim_start_matches('/')
        .chars()
        .map(|c| {
            if matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') {
                '-'
            } else {
                c
            }
        })
        .collect()
}

/// Cursor CLI 워크스페이스 신뢰 마커(`~/.cursor/projects/<slug>/.workspace-trusted`)를 생성한다(OWE-737).
pub(crate) fn pre_trust_cursor_in(home: &Path, cwd: &Path) -> io::Result<bool> {
    if is_too_broad_to_pre_trust(home, cwd) {
        return Ok(false);
    }
    let canon = std::fs::canonicalize(cwd).unwrap_or_else(|_| cwd.to_path_buf());
    let Some(canon_str) = canon.to_str().filter(|s| !s.is_empty()) else {
        return Ok(false);
    };
    let slug = cursor_project_slug(canon_str);
    if slug.is_empty() {
        return Ok(false);
    }
    let _guard = crate::session::lock(&TRUST_LOCK);
    let marker = home
        .join(".cursor/projects")
        .join(slug)
        .join(".workspace-trusted");
    if marker.exists() {
        return Ok(false);
    }
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let payload = serde_json::json!({
        "trustedAt": now_secs,
        "workspacePath": canon_str,
    });
    let mut bytes = serde_json::to_vec(&payload).map_err(io::Error::other)?;
    bytes.push(b'\n');
    write_trust_file_cas(&marker, None, &bytes, None)
}

pub(crate) fn pre_trust_workspace_in(home: &Path, cwd: &Path) -> io::Result<()> {
    if is_too_broad_to_pre_trust(home, cwd) {
        return Ok(());
    }
    let candidates = trust_candidates(cwd);
    if candidates.is_empty() {
        return Ok(());
    }
    let _guard = crate::session::lock(&TRUST_LOCK);
    let settings_path = home.join(".gemini/antigravity-cli/settings.json");
    let Ok(orig_bytes) = read_trust_file_snapshot(&settings_path) else {
        return Ok(());
    };
    let existing_str = match orig_bytes.as_deref() {
        Some(b) => {
            if let Ok(s) = std::str::from_utf8(b) {
                Some(s)
            } else {
                tracing::warn!(path = %settings_path.display(), "antigravity settings.json is not valid UTF-8; skipping");
                return Ok(());
            }
        }
        None => None,
    };
    let Some((next, changed)) = patch_antigravity_settings_json(existing_str, &candidates) else {
        tracing::warn!(path = %settings_path.display(), "skipping antigravity trust preset due to malformed JSON or unexpected schema");
        return Ok(());
    };
    if !changed {
        return Ok(());
    }
    write_trust_file_cas(&settings_path, orig_bytes.as_deref(), next.as_bytes(), None).map(|_| ())
}

/// 호스트 `~/.gemini/config/hooks.json`에 `"krill"` 훅 번들이 설치돼 있으면
/// 격리된 Antigravity 계정 프로필(`<account_home>/.gemini/config/hooks.json`)에도 `"krill"` 항목만 동기화한다.
pub(crate) fn sync_agy_profile_hooks_in(host_home: &Path, account_home: &Path) -> io::Result<bool> {
    if host_home.as_os_str().is_empty()
        || account_home.as_os_str().is_empty()
        || host_home == account_home
    {
        return Ok(false);
    }
    let host_canon = std::fs::canonicalize(host_home).unwrap_or_else(|_| host_home.to_path_buf());
    let acc_canon =
        std::fs::canonicalize(account_home).unwrap_or_else(|_| account_home.to_path_buf());
    if host_canon == acc_canon {
        return Ok(false);
    }
    let host_hooks_path = host_home.join(".gemini/config/hooks.json");
    let Ok(Some(host_bytes)) = read_trust_file_snapshot(&host_hooks_path) else {
        return Ok(false);
    };
    let Ok(host_val) = serde_json::from_slice::<serde_json::Value>(&host_bytes) else {
        return Ok(false);
    };
    let Some(krill_bundle) = host_val
        .as_object()
        .and_then(|o| o.get("krill"))
        .filter(|v| v.is_object())
    else {
        return Ok(false);
    };
    let _guard = crate::session::lock(&TRUST_LOCK);
    let target = account_home.join(".gemini/config/hooks.json");
    let Ok(orig_bytes) = read_trust_file_snapshot(&target) else {
        return Ok(false);
    };
    let mut target_val = match orig_bytes.as_deref() {
        Some(b) if !b.iter().all(u8::is_ascii_whitespace) => {
            let Ok(v) = serde_json::from_slice::<serde_json::Value>(b) else {
                return Ok(false);
            };
            v
        }
        _ => serde_json::json!({}),
    };
    let Some(target_obj) = target_val.as_object_mut() else {
        return Ok(false);
    };
    if target_obj.get("krill") == Some(krill_bundle) {
        return Ok(false);
    }
    target_obj.insert("krill".to_owned(), krill_bundle.clone());
    let next = krill_proto::agent::hooks::encode_hook_json(&target_val)?;
    write_trust_file_cas(&target, orig_bytes.as_deref(), next.as_bytes(), None)
}

/// 계정 프로필 env를 넣고 넣은 계정을 돌려준다(실행 기록에 남긴다, OWE-698).
/// `pinned`가 있으면 그 계정, 없으면 지금 활성 계정이다.
fn apply_account_env(
    login: &mut Launch,
    profile: &Profile,
    state: Option<&Path>,
    pinned: Option<&LaunchAccount>,
) -> Option<LaunchAccount> {
    let prog = profile.program()?;
    let mut account = pinned.cloned().or_else(|| active_account(state, prog));
    if (is(profile, "agy") || is(profile, "antigravity"))
        && account
            .as_ref()
            .is_some_and(|account| !account.dir.is_empty())
        && !cfg!(test)
        && !agy_gemini_dir_supported()
    {
        tracing::warn!("agy --gemini_dir is unavailable; using the host account");
        account = Some(LaunchAccount {
            id: accounts::host_default_id(ProviderKind::Antigravity),
            env_key: ProviderKind::Antigravity.env_var().to_owned(),
            dir: String::new(),
        });
    }
    if let Some(account) = &account {
        if (is(profile, "agy") || is(profile, "antigravity"))
            && !account.dir.is_empty()
            && let Some(host_home) = trust_home_for_runtime()
        {
            sync_agy_profile_hooks_in(&host_home, Path::new(&account.dir)).ok();
        }
        if let Some(provider) = ProviderKind::from_env_var(&account.env_key) {
            login
                .env
                .retain(|(key, _)| key != b"KRILL_ACCOUNT_PROVIDER" && key != b"KRILL_ACCOUNT_DIR");
            login.env.push((
                b"KRILL_ACCOUNT_PROVIDER".to_vec(),
                provider.as_str().as_bytes().to_vec(),
            ));
            login.env.push((
                b"KRILL_ACCOUNT_DIR".to_vec(),
                account.dir.as_bytes().to_vec(),
            ));
            login.env.retain(|(key, _)| {
                !provider
                    .credential_overrides()
                    .iter()
                    .any(|name| key.as_slice() == name.as_bytes())
            });
        }
        // OWE-699: 켜져 있을 때만 실행 전 멱등 프로비저닝. 실패해도 실행은 그대로 간다(경고만).
        if let Some(provider) = Path::new(prog)
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(ProviderKind::parse)
        {
            crate::usage::provision::provision_if_enabled(
                &accounts::default_home(),
                state,
                provider,
                &account.dir,
            );
        }
        if is(profile, "opencode") && !account.dir.is_empty() {
            login
                .env
                .push((b"OPENCODE_DB".to_vec(), b"opencode.db".to_vec()));
            for (key, suffix) in [("XDG_DATA_HOME", "data"), ("XDG_STATE_HOME", "state")] {
                login.env.retain(|(k, _)| k.as_slice() != key.as_bytes());
                login.env.push((
                    key.as_bytes().to_vec(),
                    Path::new(&account.dir)
                        .join(suffix)
                        .into_os_string()
                        .as_encoded_bytes()
                        .to_vec(),
                ));
            }
            login
                .env
                .retain(|(k, _)| k.as_slice() != b"OPENCODE_CONFIG_DIR");
        } else if !is(profile, "agy") && !is(profile, "antigravity") {
            let key = account.env_key.as_bytes();
            login.env.retain(|(k, _)| k.as_slice() != key);
            if !account.dir.is_empty() {
                login
                    .env
                    .push((key.to_vec(), account.dir.as_bytes().to_vec()));
            }
        }
    }
    // User environment remains customizable; credential authority belongs to the selected account.
    for (key, value) in &profile.env {
        if let Some(provider) = account
            .as_ref()
            .and_then(|a| ProviderKind::from_env_var(&a.env_key))
            && (key == provider.env_var()
                || matches!(key.as_str(), "KRILL_ACCOUNT_PROVIDER" | "KRILL_ACCOUNT_DIR")
                || provider.credential_overrides().contains(&key.as_str())
                || account.as_ref().is_some_and(|a| !a.dir.is_empty())
                    && provider == ProviderKind::OpenCode
                    && matches!(key.as_str(), "XDG_DATA_HOME" | "XDG_STATE_HOME"))
        {
            continue;
        }
        login.env.retain(|(k, _)| k.as_slice() != key.as_bytes());
        login
            .env
            .push((key.as_bytes().to_vec(), value.as_bytes().to_vec()));
    }
    if let Some(state) = state {
        inject_zsh_env(login, state);
    }
    account
}

/// 숨은 agy 플래그가 사라진 업데이트 뒤 잘못된 프로필로 조용히 실행하지 않도록 격리 HOME에서 확인한다.
///
/// 결과는 (실행 경로, mtime, 길이, `--version` 문자열) 키로 캐시한다(OWE-901).
/// 키에 버전이 들어 있어 agy 업그레이드하면 자동으로 다시 보고, 실패해도 최대 1시간만 기억한다.
/// 호스트 폴백(`apply_account_env`) 알림은 기존 warn 로그 + 기록 계정(호스트 id)이 맡는다.
/// 전용 배너·세션 배지는 새 IPC가 필요해 넣지 않았다(OWE-901).
fn agy_gemini_dir_supported() -> bool {
    use krill_proto::os::ffi::OsStrExt as _;
    let path = std::ffi::OsStr::from_bytes(crate::shell::login_path());
    let Some(exe) = krill_proto::agent::which("agy", path) else {
        return false;
    };
    agy_gemini_dir_supported_cached(&exe)
}

#[cfg(test)]
#[cfg(unix)]
pub(crate) fn agy_guard_cache_len_for_tests() -> usize {
    crate::session::lock(&AGY_GUARD_CACHE).len()
}

#[cfg(test)]
#[cfg(unix)]
pub(crate) fn clear_agy_guard_cache_for_tests() {
    crate::session::lock(&AGY_GUARD_CACHE).clear();
}

fn agy_gemini_dir_supported_cached(exe: &Path) -> bool {
    let (mtime, len) = std::fs::symlink_metadata(exe)
        .and_then(|m| {
            m.modified().map(|t| {
                (
                    t.duration_since(std::time::UNIX_EPOCH)
                        .map_or(0, |d| d.as_secs()),
                    m.len(),
                )
            })
        })
        .unwrap_or((0, 0));
    let version = crate::usage::direct::agy::version_text_for_guard(exe);
    let key = (exe.to_path_buf(), mtime, len, version);
    if let Some(supported) = crate::session::lock(&AGY_GUARD_CACHE)
        .get(&key)
        .filter(|(at, _)| at.elapsed() < AGY_GUARD_TTL)
        .map(|(_, supported)| *supported)
    {
        return supported;
    }
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let root = std::env::temp_dir().join(format!("krill-agy-guard-{}-{nonce}", std::process::id()));
    let supported = agy_gemini_dir_supported_with(exe, &root);
    let mut cache = crate::session::lock(&AGY_GUARD_CACHE);
    if cache.len() >= 64 {
        cache.pop_first();
    }
    cache.insert(key, (Instant::now(), supported));
    supported
}

/// agy 가드 결과 캐시(OWE-901). 키는 (실행 경로, mtime, 길이, 버전 문자열).
type AgyGuardKey = (PathBuf, u64, u64, String);
static AGY_GUARD_CACHE: std::sync::Mutex<std::collections::BTreeMap<AgyGuardKey, (Instant, bool)>> =
    std::sync::Mutex::new(std::collections::BTreeMap::new());
/// 가드 결과 기억 상한. 키가 바뀌면(업그레이드) 그 전에 다시 본다.
const AGY_GUARD_TTL: Duration = Duration::from_hours(1);
/// 가드 프로브(`agy --gemini_dir=… mcp list`) 대기 상한. 테스트는 전체 검사의 병렬 부하에서 갓 만든 가짜 스크립트
/// 실행이 5초를 넘겨(2026-10-20 실측 5.1·9.6초) 지원 안 함으로 잘못 판정해 30초로 둔다.
const AGY_GUARD_PROBE_TIMEOUT: Duration = if cfg!(test) {
    Duration::from_secs(30)
} else {
    Duration::from_secs(5)
};

fn agy_gemini_dir_supported_with(exe: &Path, root: &Path) -> bool {
    use krill_proto::os::fs::DirBuilderExt as _;
    if std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(root.join("home"))
        .is_err()
    {
        return false;
    }
    let profile = root.join("profile/.gemini");
    let Ok(mut child) = Command::new(exe)
        .arg(format!("--gemini_dir={}", profile.display()))
        .args(["mcp", "list"])
        .env("HOME", root.join("home"))
        .env("SSH_CONNECTION", "krill")
        .env(
            "PATH",
            std::ffi::OsStr::from_bytes(crate::shell::login_path()),
        )
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        std::fs::remove_dir_all(root).ok();
        return false;
    };
    let deadline = Instant::now() + AGY_GUARD_PROBE_TIMEOUT;
    let supported = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status.success(),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            _ => {
                child.kill().ok();
                child.wait().ok();
                break false;
            }
        }
    };
    std::fs::remove_dir_all(root).ok();
    supported
}

fn mk_bin() -> io::Result<PathBuf> {
    let sibling = krill_proto::daemon::sibling_exe("krill")?;
    if sibling.exists() {
        return Ok(sibling);
    }
    let exe = std::env::current_exe()?;
    if let Some(parent) = exe
        .parent()
        .and_then(Path::parent)
        .map(|p| p.join("krill"))
        .filter(|p| p.exists())
    {
        return Ok(parent);
    }
    // 통합·단위 테스트 바이너리(`target/debug/deps/...`)에서 `krill`가 아직 없을 때도 canonicalize가 성공하게 한다.
    Ok(exe)
}

/// 모든 세션(셸·에이전트)의 자식 환경에 더하는 값. 설치한 전역 hook은 `MK_PANE_ID`가 있을 때만 `krill hook`을 부르고
/// (그 밖에서는 no-op), `krill hook`은 `KRILL_STATE_DIR`의 소켓·spool을 쓴다. 세션 귀속은 값이 아니라 PID 계보로 한다.
pub fn pane_env(session: krill_proto::SessionId) -> Vec<(Vec<u8>, Vec<u8>)> {
    pane_env_in(session, krill_proto::daemon::state_dir().as_deref())
}

pub(crate) fn pane_env_in(
    session: krill_proto::SessionId,
    state: Option<&Path>,
) -> Vec<(Vec<u8>, Vec<u8>)> {
    let mut env = vec![(b"MK_PANE_ID".to_vec(), session.0.to_string().into_bytes())];
    if let Ok(executable) = mk_bin() {
        env.push((
            b"KRILL_CLI".to_vec(),
            executable.as_os_str().as_bytes().to_vec(),
        ));
    }
    if let Some(state) = state {
        env.push((
            b"KRILL_STATE_DIR".to_vec(),
            state.as_os_str().as_bytes().to_vec(),
        ));
    }
    env
}

/// 프로그램 파일명이 `name`인가(`/opt/bin/codex`도 `codex`).
fn is(profile: &Profile, name: &str) -> bool {
    profile
        .program()
        .is_some_and(|p| Path::new(p).file_name().is_some_and(|n| n == name))
}

/// Codex 프로필이면 프로그램 바로 뒤에 `-c` hook을 넣은 사본. 명령은 설치가 끝난(`complete`, trust 등록 포함)
/// receipt의 것 그대로다. Codex가 아니거나 receipt가 없거나 깨졌으면 `None`(hook 없이 띄운다).
/// trust는 `CODEX_HOME`마다 따로라 `env`(계정 적용 뒤 실행 env)가 가리키는 계정 홈에도 그 명령을 신뢰시킨다.
fn codex_hooks(
    profile: &Profile,
    state: Option<&Path>,
    env: &[(Vec<u8>, Vec<u8>)],
) -> io::Result<Option<Profile>> {
    if !is(profile, "codex") {
        return Ok(None);
    }
    let receipt = state
        .and_then(|state| std::fs::read(hooks::receipt(state, Agent::Codex)).ok())
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok());
    let Some(receipt) =
        receipt.filter(|r| r.get("complete").and_then(serde_json::Value::as_bool) == Some(true))
    else {
        return Ok(None);
    };
    let Some(command) = receipt
        .pointer("/codex/command")
        .and_then(serde_json::Value::as_str)
    else {
        return Ok(None);
    };
    codex::ensure_trust(
        profile.program(),
        receipt
            .pointer("/codex/path")
            .and_then(serde_json::Value::as_str),
        command,
        env,
    )?;
    let mut profile = profile.clone();
    profile.launch.splice(1..1, hooks::codex_args(command));
    Ok(Some(profile))
}

/// 로그인 셸 실행([`Shell::Login`])을 에이전트 실행으로 바꾼다. env는 건드리지 않는다.
fn wrap(profile: &Profile, permission: Permission, login: Launch) -> Launch {
    wrap_account(profile, permission, login, None)
}

fn wrap_account(
    profile: &Profile,
    permission: Permission,
    login: Launch,
    account: Option<&LaunchAccount>,
) -> Launch {
    skills::prepare(Some(profile), &login, account);
    let hint = format!(
        "{}: not installed. install: {}",
        profile.program().unwrap_or_default(),
        profile.install_command()
    );
    let mut agent_argv = profile.argv(permission);
    if is(profile, "codex")
        && std::env::var_os("KRILL_CODEX_ISOLATE").is_none_or(|v| v != "0")
        && !agent_argv.iter().skip(1).any(|a| {
            matches!(a.as_str(), "--no-daemon" | "agents" | "queue" | "--remote")
                || a.starts_with("--remote=")
        })
        && codex::no_daemon_supported(profile.program())
    {
        agent_argv.insert(1, "--no-daemon".to_owned());
    }
    if (is(profile, "agy") || is(profile, "antigravity"))
        && let Some(account) = account.filter(|a| !a.dir.is_empty())
    {
        let gemini_dir = Path::new(&account.dir).join(".gemini");
        agent_argv.insert(1, format!("--gemini_dir={}", gemini_dir.display()));
    }
    #[cfg(windows)]
    {
        windows::wrap(login, &hint, agent_argv)
    }
    #[cfg(unix)]
    {
        let Launch { mut argv, env } = login;
        argv.truncate(1);
        argv.extend(
            ["-l", "-i", "-c", PREFLIGHT, "mk-agent", &hint]
                .into_iter()
                .map(|s| s.as_bytes().to_vec()),
        );
        argv.extend(agent_argv.into_iter().map(String::into_bytes));
        Launch { argv, env }
    }
}

#[cfg(test)]
#[cfg(unix)]
mod tests;
