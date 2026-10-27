//! 번들 2단계 원자 교체·단계별 저널·별도 relauncher·health 확인·롤백·시작 대화상자(MK-063).

use std::fs::{self, DirBuilder};
use std::io;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, ExitCode, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use gpui::{App, AppContext as _, PromptLevel, Window};
use krill_proto::os::fs::DirBuilderExt as _;
#[cfg(unix)]
use rustix::process::{Pid, test_kill_process};
use serde::{Deserialize, Serialize};

use super::{
    APP_EXE, CodeVerifier, CodesignVerifier, Config, Error, READY_ENV, RELAUNCH_ARG, UPDATE_DIR,
    append_ext, bundle_short_version, journal_path, prev_path, ready_path, rename, staging_lock,
    staging_path, trash_path, write_private_file,
};

const RELAUNCHER_BIN: &str = "relauncher";
const POLL_INTERVAL: Duration = Duration::from_millis(20);
const WAIT_PID_TIMEOUT: Duration = Duration::from_secs(15);

pub(super) use crate::i18n::catalog::updater_relaunch as text;

/// 롤백 사유 영문을 사용자용 문구로 바꾼다(OWE-883). 모르는 사유는 그대로 둔다.
#[must_use]
pub fn localize_reason(reason: &str) -> String {
    if reason == "interrupted during bundle replacement" {
        return text::REASON_INTERRUPTED.get().to_owned();
    }
    if let Some(status) = reason.strip_prefix("new app exited before ready (") {
        let status = status.strip_suffix(')').unwrap_or(status);
        return text::REASON_EXITED_EARLY.format(&[("status", status)]);
    }
    if let Some(ms) = reason.strip_prefix("new app did not report ready within ") {
        let ms = ms.strip_suffix("ms").unwrap_or(ms);
        return text::REASON_NO_READY_SIGNAL.format(&[("ms", ms)]);
    }
    if reason == "old app did not exit; replacement aborted" {
        return text::REASON_OLD_APP_LINGERING.get().to_owned();
    }
    reason.to_owned()
}

/// 교체·재실행 단계별 저널(ARCHITECTURE §7).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Journal {
    pub bundle: PathBuf,
    pub from_version: String,
    pub to_version: String,
    pub health_timeout_ms: u64,
    pub stage: Stage,
}

/// 저널 단계.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum Stage {
    /// 1단계 이동 전: 현재 `<번들>` → `<번들>.prev`.
    MovingToPrev,
    /// 2단계 이동 전: `<번들>.staging` → `<번들>`.
    MovingStagingToTarget,
    /// 교체 완료, 새 앱 기동 신호(`ready`) 대기 중.
    VerifyingLaunch,
    /// 새 앱이 기동 신호를 보냄(업데이트 확정).
    Healthy,
    /// `<번들>.prev`로 자동 또는 수동 롤백 완료(다음 실행에 안내 대화상자 표시).
    RolledBack { reason: String },
    /// 사용자가 롤백 안내를 확인함.
    Acknowledged,
}

impl Journal {
    pub fn load(state_dir: &Path) -> Result<Option<Self>, Error> {
        let path = journal_path(state_dir);
        match fs::read_to_string(path) {
            Ok(s) => serde_json::from_str(&s)
                .map(Some)
                .map_err(|e| Error::Json(e.to_string())),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(Error::Io(e)),
        }
    }

    pub fn save(&self, state_dir: &Path) -> Result<(), Error> {
        let dir = state_dir.join(UPDATE_DIR);
        DirBuilder::new().recursive(true).mode(0o700).create(&dir)?;
        let path = journal_path(state_dir);
        let tmp = append_ext(&path, "tmp");
        let bytes = serde_json::to_vec_pretty(self).map_err(|e| Error::Json(e.to_string()))?;
        write_private_file(&tmp, &bytes)?;
        fs::rename(tmp, path)?;
        Ok(())
    }
}

/// 번들 교체 방식.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Mode {
    /// macOS: `.app` 디렉터리 inode를 보존하고 일반 파일만 새 vnode로 바꾸는 제자리 동기화.
    InPlace,
    /// Windows: 실행 중 exe를 덮어쓸 수 없어 파일 단위로 `.prev`에 옮기고 새 파일을 놓는다([`rename`]).
    Rename,
}

pub(super) const MODE: Mode = if cfg!(windows) {
    Mode::Rename
} else {
    Mode::InPlace
};

/// `<app_bundle>.staging`을 검증한 뒤 `.prev` 백업을 만들고 새 버전으로 교체한다.
/// macOS는 제자리 동기화(`staging -> 현재`)로 `.app` 디렉터리 inode는 보존하고 일반 파일은 새 vnode로
/// 원자 교체(Apple Silicon XNU `cs_blob` `SIGKILL` 방지)한다. Windows는 파일 단위 이름 바꾸기([`Mode::Rename`]).
pub fn swap_bundles(
    app_bundle: &Path,
    state_dir: &Path,
    from_version: &str,
    to_version: &str,
    health_timeout: Duration,
    verifier: &dyn CodeVerifier,
) -> Result<Journal, Error> {
    swap_with(
        MODE,
        app_bundle,
        state_dir,
        from_version,
        to_version,
        health_timeout,
        verifier,
    )
}

pub(super) fn swap_with(
    mode: Mode,
    app_bundle: &Path,
    state_dir: &Path,
    from_version: &str,
    to_version: &str,
    health_timeout: Duration,
    verifier: &dyn CodeVerifier,
) -> Result<Journal, Error> {
    let _staging_lock = staging_lock(app_bundle)?;
    let staging = staging_path(app_bundle);
    let prev = prev_path(app_bundle);
    let trash = trash_path(app_bundle);
    if bundle_short_version(&staging).as_deref() != Some(to_version) {
        return Err(Error::Config(text::CARD_PAYLOAD_CHANGED.get().to_owned()));
    }
    verifier.verify(&staging)?;

    match mode {
        Mode::InPlace if prev.exists() => fs::remove_dir_all(&prev)?,
        Mode::InPlace => {}
        Mode::Rename => {
            rename::check_install_entries(app_bundle, &staging)?;
            rename::purge(&trash);
            rename::clear(&prev, &trash)?;
        }
    }
    let timeout_ms = u64::try_from(health_timeout.as_millis()).unwrap_or(u64::MAX);
    let mut journal = Journal {
        bundle: app_bundle.to_path_buf(),
        from_version: from_version.to_owned(),
        to_version: to_version.to_owned(),
        health_timeout_ms: timeout_ms,
        stage: Stage::MovingToPrev,
    };
    journal.save(state_dir)?;

    let replaced = (|| -> Result<(), Error> {
        if !app_bundle.exists() || !staging.exists() {
            return Err(Error::Io(io::Error::new(
                io::ErrorKind::NotFound,
                "source or staging bundle missing",
            )));
        }
        match mode {
            Mode::InPlace => sync_bundle_in_place(app_bundle, &prev)?,
            Mode::Rename => rename::move_tree(app_bundle, &prev, &trash)?,
        }
        journal.stage = Stage::MovingStagingToTarget;
        journal.save(state_dir)?;
        match mode {
            Mode::InPlace => sync_bundle_in_place(&staging, app_bundle)?,
            Mode::Rename => rename::move_tree(&staging, app_bundle, &trash)?,
        }
        fs::remove_dir_all(&staging)?;
        journal.stage = Stage::VerifyingLaunch;
        journal.save(state_dir)
    })();
    if let Err(error) = replaced {
        // 어느 단계가 실패해도 원래 번들을 복원한 후에만 RolledBack을 기록한다.
        if prev.exists() {
            if app_bundle.exists() {
                rollback_with(mode, app_bundle, state_dir, &error.to_string())?;
            } else {
                fs::rename(&prev, app_bundle)?;
            }
        }
        journal.stage = Stage::RolledBack {
            reason: error.to_string(),
        };
        journal.save(state_dir)?;
        return Err(error);
    }

    Ok(journal)
}

fn sync_bundle_in_place(src: &Path, dst: &Path) -> io::Result<()> {
    fs::create_dir_all(dst)?;
    copy_dir_in_place(src, dst)?;
    prune_extraneous_entries(src, dst)
}

fn copy_dir_in_place(src: &Path, dst: &Path) -> io::Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let ft = entry.file_type()?;
        let s = entry.path();
        let t = dst.join(entry.file_name());
        if ft.is_dir() {
            copy_dir_in_place(&s, &t)?;
        } else if ft.is_symlink() {
            if t.symlink_metadata().is_ok() {
                fs::remove_file(&t)?;
            }
            let target = fs::read_link(&s)?;
            #[cfg(unix)]
            std::os::unix::fs::symlink(target, &t)?;
            #[cfg(not(unix))]
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                format!("bundle symlink {}", target.display()),
            ));
        } else {
            let file_name = entry.file_name();
            let tmp = dst.join(format!(
                ".{}.tmp.{}",
                file_name.to_string_lossy(),
                std::process::id()
            ));
            fs::copy(&s, &tmp)?;
            if let Err(e) = fs::set_permissions(&tmp, entry.metadata()?.permissions()) {
                fs::remove_file(&tmp).ok();
                return Err(e);
            }
            if let Err(e) = fs::rename(&tmp, &t) {
                fs::remove_file(&tmp).ok();
                return Err(e);
            }
        }
    }
    Ok(())
}

fn prune_extraneous_entries(src: &Path, dst: &Path) -> io::Result<()> {
    for entry in fs::read_dir(dst)? {
        let entry = entry?;
        let ft = entry.file_type()?;
        let s = src.join(entry.file_name());
        let t = entry.path();
        if !s.exists() && s.symlink_metadata().is_err() {
            if ft.is_dir() {
                fs::remove_dir_all(&t)?;
            } else {
                fs::remove_file(&t)?;
            }
        } else if ft.is_dir() && s.is_dir() {
            prune_extraneous_entries(&s, &t)?;
        }
    }
    Ok(())
}

/// 교체 도중 크래시로 멈춘 저널(`MovingToPrev`·`MovingStagingToTarget`)을 복구한다.
///
/// - 번들이 없고 백업이 있으면 복원하고 `RolledBack`을 기록한다(진짜 롤백).
/// - 번들이 있고 스테이징이 없으면 교체가 끝난 것이다. 성공한 교체를
///   `RolledBack`으로 오표기하지 않고(AS-09) 검증 단계로 이어간다.
/// - 번들·스테이징이 둘 다 멀쩡하면 교체 전에 죽은 것이다. 복원할 게 없으니
///   거짓 롤백 대화상자를 남기지 않고 저널을 버린다.
///
/// Windows([`Mode::Rename`])는 설치 폴더가 사라지지 않으므로 단계로 판정한다. 옮기기 전(`MovingToPrev`)에
/// 멈췄으면 `.prev`의 파일을 되돌리고 저널을 버린다. 새 파일을 놓다가(`MovingStagingToTarget`) 멈췄으면
/// 이미 검증된 남은 스테이징을 마저 옮겨 검증 단계로 이어간다.
pub fn recover_journal(state_dir: &Path) -> Result<Option<Journal>, Error> {
    recover_with(MODE, state_dir)
}

pub(super) fn recover_with(mode: Mode, state_dir: &Path) -> Result<Option<Journal>, Error> {
    let Some(mut journal) = Journal::load(state_dir)? else {
        return Ok(None);
    };
    if matches!(
        journal.stage,
        Stage::MovingToPrev | Stage::MovingStagingToTarget
    ) {
        let staging = staging_path(&journal.bundle);
        let prev = prev_path(&journal.bundle);
        if mode == Mode::Rename {
            let trash = trash_path(&journal.bundle);
            if journal.stage == Stage::MovingToPrev {
                if prev.exists() {
                    rename::move_tree(&prev, &journal.bundle, &trash)?;
                }
                fs::remove_file(journal_path(state_dir)).ok();
                return Ok(None);
            }
            if staging.exists() {
                rename::move_tree(&staging, &journal.bundle, &trash)?;
                fs::remove_dir_all(&staging)?;
            }
            journal.stage = Stage::VerifyingLaunch;
            journal.save(state_dir)?;
            return Ok(Some(journal));
        }
        if !journal.bundle.exists() && prev.exists() {
            fs::rename(&prev, &journal.bundle)?;
            journal.stage = Stage::RolledBack {
                reason: "interrupted during bundle replacement".to_owned(),
            };
            journal.save(state_dir)?;
            return Ok(Some(journal));
        }
        if journal.bundle.exists() && !staging.exists() {
            journal.stage = Stage::VerifyingLaunch;
            journal.save(state_dir)?;
            return Ok(Some(journal));
        }
        if journal.bundle.exists() && staging.exists() {
            fs::remove_file(journal_path(state_dir)).ok();
            return Ok(None);
        }
        journal.stage = Stage::RolledBack {
            reason: "interrupted during bundle replacement".to_owned(),
        };
        journal.save(state_dir)?;
    }
    Ok(Some(journal))
}

/// `<app_bundle>.prev` 백업으로 `<app_bundle>`을 롤백하고 저널을 `Stage::RolledBack`으로 갱신한다.
pub fn rollback(app_bundle: &Path, state_dir: &Path, reason: &str) -> Result<(), Error> {
    rollback_with(MODE, app_bundle, state_dir, reason)
}

pub(super) fn rollback_with(
    mode: Mode,
    app_bundle: &Path,
    state_dir: &Path,
    reason: &str,
) -> Result<(), Error> {
    let prev = prev_path(app_bundle);
    if !prev.exists() {
        return Err(Error::NoBackup(prev));
    }
    if mode == Mode::Rename {
        // 옮기는 도중(`MovingToPrev`) 실패면 설치 폴더에 남은 것은 아직 안 옮긴 구 파일이라 지우지 않는다.
        // 그 뒤 단계면 설치 폴더의 파일은 새 버전이므로 치우고 구 파일을 되돌린다.
        let trash = trash_path(app_bundle);
        let partial =
            matches!(Journal::load(state_dir), Ok(Some(j)) if j.stage == Stage::MovingToPrev);
        if !partial {
            rename::clear(app_bundle, &trash)?;
        }
        rename::move_tree(&prev, app_bundle, &trash)?;
        fs::remove_dir_all(&prev).ok();
    } else if app_bundle.exists() {
        sync_bundle_in_place(&prev, app_bundle)?;
        fs::remove_dir_all(&prev)?;
    } else {
        fs::rename(&prev, app_bundle)?;
    }

    if let Ok(Some(mut journal)) = Journal::load(state_dir) {
        journal.stage = Stage::RolledBack {
            reason: reason.to_owned(),
        };
        journal.save(state_dir)?;
    }
    Ok(())
}

/// relauncher 실행 결과.
#[derive(Debug)]
pub enum RelaunchOutcome {
    /// 새 버전이 기동 확인 신호(`ready`)를 보내 업데이트가 완료됨.
    Updated { child: Child },
    /// 새 버전이 종료·타임아웃되어 이전 버전(`.prev`)으로 롤백 후 이전 버전을 재기동함.
    RolledBack { reason: String, restored: Child },
}

/// 별도 relauncher가 수행하는 전체 교체·재실행·health 확인·롤백 절차.
///
/// 1. `wait_pid`가 주어지면 구 앱 프로세스가 끝날 때까지 기다린다.
/// 2. [`swap_bundles`]로 `<번들>` → `<번들>.prev`, `<번들>.staging` → `<번들>` 원자 교체.
/// 3. 새 `<번들>/Contents/MacOS/krill-app`을 띄우고 `health_timeout` 안에 [`ready_path`]가 생기는지 감시한다.
/// 4. 새 앱이 먼저 종료되거나 타임아웃되면 새 앱을 정리하고 [`rollback`]으로 `<번들>.prev`를 복원한 뒤 다시 띄운다.
pub fn relaunch_and_verify(
    app_bundle: &Path,
    state_dir: &Path,
    from_version: &str,
    to_version: &str,
    wait_pid: Option<u32>,
    health_timeout: Duration,
    verifier: &dyn CodeVerifier,
) -> Result<RelaunchOutcome, Error> {
    if let Some(pid) = wait_pid {
        wait_process_exit(pid, WAIT_PID_TIMEOUT)?;
    }
    if let Err(error) = swap_bundles(
        app_bundle,
        state_dir,
        from_version,
        to_version,
        health_timeout,
        verifier,
    ) {
        if let Some(journal) = Journal::load(state_dir)?
            && journal.from_version == from_version
            && journal.to_version == to_version
            && matches!(journal.stage, Stage::RolledBack { .. })
        {
            let restored = spawn_bundle_app(app_bundle, state_dir, None)?;
            return Ok(RelaunchOutcome::RolledBack {
                reason: error.to_string(),
                restored,
            });
        }
        return Err(error);
    }

    let ready = ready_path(state_dir, to_version);
    fs::remove_file(&ready).ok();
    let mut child = spawn_bundle_app(app_bundle, state_dir, Some(&ready))?;
    let deadline = Instant::now()
        .checked_add(health_timeout)
        .unwrap_or_else(Instant::now);

    let fail_reason = loop {
        if ready.is_file() {
            if let Ok(Some(mut j)) = Journal::load(state_dir) {
                j.stage = Stage::Healthy;
                j.save(state_dir)?;
            }
            return Ok(RelaunchOutcome::Updated { child });
        }
        if let Some(status) = child.try_wait()? {
            break format!("new app exited before ready ({status})");
        }
        if Instant::now() >= deadline {
            break format!(
                "new app did not report ready within {}ms",
                health_timeout.as_millis()
            );
        }
        thread::sleep(POLL_INTERVAL);
    };

    child.kill().ok();
    child.wait().ok();
    rollback(app_bundle, state_dir, &fail_reason)?;
    let restored = spawn_bundle_app(app_bundle, state_dir, None)?;
    Ok(RelaunchOutcome::RolledBack {
        reason: fail_reason,
        restored,
    })
}

fn spawn_bundle_app(
    app_bundle: &Path,
    state_dir: &Path,
    ready_file: Option<&Path>,
) -> io::Result<Child> {
    let mut cmd = crate::platform::background_command(app_bundle.join(APP_EXE));
    set_relaunch_state(
        &mut cmd,
        state_dir,
        krill_proto::daemon::default_state_dir().as_deref(),
        krill_proto::daemon::state_dir_override().is_some(),
    );
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Some(ready) = ready_file {
        cmd.env(READY_ENV, ready);
    }
    cmd.spawn()
}

/// Keep explicit isolation, but do not turn a normal install into an isolated
/// one merely because it was restarted by the updater.
fn set_relaunch_state(
    command: &mut std::process::Command,
    state: &Path,
    default: Option<&Path>,
    explicitly_overridden: bool,
) {
    if explicitly_overridden
        || default.is_none_or(|default| !krill_proto::path::equivalent(state, default))
    {
        command.env("KRILL_STATE_DIR", state);
    }
}

#[cfg(unix)]
fn wait_process_exit(raw_pid: u32, timeout: Duration) -> io::Result<()> {
    let pid = i32::try_from(raw_pid)
        .ok()
        .and_then(Pid::from_raw)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "invalid old app pid"))?;
    let deadline = Instant::now()
        .checked_add(timeout)
        .unwrap_or_else(Instant::now);
    loop {
        match test_kill_process(pid) {
            Err(rustix::io::Errno::SRCH) => return Ok(()),
            Err(e) => return Err(e.into()),
            Ok(()) => {}
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "old app did not exit; replacement aborted",
            ));
        }
        thread::sleep(POLL_INTERVAL);
    }
}

/// Windows: `tasklist`로 PID가 사라질 때까지 기다린다(unsafe FFI는 platform 모듈 밖에 두지 않는다, AGENTS 5).
// ponytail: tasklist 한 번에 수십 ms. 대기는 업데이트 때 한 번뿐이라 OpenProcess+WaitForSingleObject로 바꾸지 않는다.
#[cfg(windows)]
fn wait_process_exit(raw_pid: u32, timeout: Duration) -> io::Result<()> {
    use std::os::windows::process::CommandExt as _;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let filter = format!("PID eq {raw_pid}");
    let needle = format!("\"{raw_pid}\"");
    let deadline = Instant::now()
        .checked_add(timeout)
        .unwrap_or_else(Instant::now);
    loop {
        let out = crate::platform::background_command("tasklist")
            .args(["/FI", &filter, "/NH", "/FO", "CSV"])
            .creation_flags(CREATE_NO_WINDOW)
            .stdin(Stdio::null())
            .output()?;
        if !String::from_utf8_lossy(&out.stdout).contains(&needle) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "old app did not exit; replacement aborted",
            ));
        }
        thread::sleep(POLL_INTERVAL);
    }
}

#[cfg(not(any(unix, windows)))]
fn wait_process_exit(_raw_pid: u32, _timeout: Duration) -> io::Result<()> {
    Err(io::ErrorKind::Unsupported.into())
}

/// 현재 실행 파일을 `<state_dir>/update/relauncher`로 복사한 뒤 별도 프로세스로 띄운다.
pub fn spawn_detached_relauncher(
    relauncher_source_exe: &Path,
    app_bundle: &Path,
    state_dir: &Path,
    from_version: &str,
    to_version: &str,
    wait_pid: Option<u32>,
    health_timeout: Duration,
) -> Result<Child, Error> {
    let dir = state_dir.join(UPDATE_DIR);
    DirBuilder::new().recursive(true).mode(0o700).create(&dir)?;
    let bin = dir.join(format!("{RELAUNCHER_BIN}{}", std::env::consts::EXE_SUFFIX));
    fs::remove_file(&bin).ok();
    fs::copy(relauncher_source_exe, &bin)?;
    #[cfg(unix)]
    fs::set_permissions(&bin, fs::Permissions::from_mode(0o700))?;

    let mut cmd = crate::platform::background_command(&bin);
    // Windows: 앱 콘솔·프로세스 그룹과 분리한다(앱이 끝나도 relauncher는 남아야 한다, OWE-829).
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
    }
    cmd.arg(RELAUNCH_ARG)
        .arg(app_bundle)
        .arg(state_dir)
        .arg(from_version)
        .arg(to_version)
        .arg(health_timeout.as_millis().to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Some(pid) = wait_pid {
        cmd.arg(pid.to_string());
    }
    Ok(cmd.spawn()?)
}

/// `krill-app --relaunch-update <bundle> <state_dir> <from> <to> <timeout_ms> [wait_pid]` CLI 진입점.
pub fn maybe_run_relauncher_cli() -> Option<ExitCode> {
    let mut args = std::env::args_os().skip(1);
    if args.next().is_none_or(|a| a != RELAUNCH_ARG) {
        return None;
    }
    let code = match run_relauncher_args(args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            tracing::error!(%e, "relauncher failed");
            ExitCode::FAILURE
        }
    };
    Some(code)
}

pub(super) fn run_relauncher_args(
    mut args: impl Iterator<Item = std::ffi::OsString>,
) -> Result<(), Error> {
    let bundle = PathBuf::from(
        args.next()
            .ok_or_else(|| Error::Config("missing bundle arg".to_owned()))?,
    );
    let state_dir = PathBuf::from(
        args.next()
            .ok_or_else(|| Error::Config("missing state_dir arg".to_owned()))?,
    );
    let from = args
        .next()
        .and_then(|s| s.into_string().ok())
        .ok_or_else(|| Error::Config("missing from_version arg".to_owned()))?;
    let to = args
        .next()
        .and_then(|s| s.into_string().ok())
        .ok_or_else(|| Error::Config("missing to_version arg".to_owned()))?;
    let timeout_ms: u64 = args
        .next()
        .and_then(|s| s.into_string().ok())
        .and_then(|s| s.parse().ok())
        .unwrap_or(30_000);
    let wait_pid: Option<u32> = args
        .next()
        .and_then(|s| s.into_string().ok())
        .and_then(|s| s.parse().ok());

    let cfg = Config::load_for_bundle(&state_dir, Some(&bundle))?;
    let _ = cfg.public_key_bytes()?;
    let verifier = CodesignVerifier::from_config(&cfg);
    let _outcome = relaunch_and_verify(
        &bundle,
        &state_dir,
        &from,
        &to,
        wait_pid,
        Duration::from_millis(timeout_ms),
        &verifier,
    )?;
    Ok(())
}

/// 앱이 창과 데몬 연결을 준비한 직후 호출해 relauncher에 정상 기동 신호(`ready`)를 보낸다.
pub fn mark_ready(state_dir: &Path, current_version: &str) {
    if let Some(path) = std::env::var_os(READY_ENV) {
        write_private_file(&PathBuf::from(path), b"ok\n").ok();
    }
    write_private_file(&ready_path(state_dir, current_version), b"ok\n").ok();
    let bundle_ver = std::env::current_exe()
        .ok()
        .and_then(|exe| super::current_app_bundle_in(None, &exe))
        .and_then(|b| super::bundle_short_version(&b));
    if let Some(ref bv) = bundle_ver
        && bv != current_version
    {
        write_private_file(&ready_path(state_dir, bv), b"ok\n").ok();
    }
    if let Ok(Some(mut journal)) = Journal::load(state_dir)
        && journal.stage == Stage::VerifyingLaunch
        && (journal.to_version == current_version
            || bundle_ver.as_deref() == Some(journal.to_version.as_str()))
    {
        journal.stage = Stage::Healthy;
        journal.save(state_dir).ok();
    }
}

/// 시작 시 중간 크래시 저널을 복구하고 롤백 대화상자를 띄운다. ready는 첫 Attached 뒤에 보낸다.
pub fn on_startup(state_dir: PathBuf, window: &mut Window, cx: &mut App) {
    load_rollback(state_dir, true, window, cx);
}

/// Load the journal without waiting on disk on the event loop.
pub fn offer_rollback(state_dir: PathBuf, window: &mut Window, cx: &mut App) {
    load_rollback(state_dir, false, window, cx);
}

fn load_rollback(state_dir: PathBuf, recover: bool, window: &Window, cx: &mut App) {
    let handle = window.window_handle();
    let work_dir = state_dir.clone();
    let work = cx.background_spawn(async move {
        if recover {
            recover_journal(&work_dir).ok();
        }
        Journal::load(&work_dir).ok().flatten()
    });
    cx.spawn(async move |cx| {
        let Some(journal) = work.await else {
            return;
        };
        let Stage::RolledBack { reason } = &journal.stage else {
            return;
        };
        let answer = handle.update(cx, |_, window, cx| {
            let detail = text::ROLLBACK_DONE_DETAIL
                .get()
                .replace("{to}", &journal.to_version)
                .replace("{from}", &journal.from_version)
                .replace("{reason}", &localize_reason(reason));
            window.prompt(
                PromptLevel::Warning,
                text::ROLLBACK_DONE_TITLE.get(),
                Some(&detail),
                &[text::OK_BUTTON.get()],
                cx,
            )
        });
        let Ok(answer) = answer else {
            return;
        };
        if answer.await.is_err() {
            return;
        }
        cx.background_executor()
            .spawn(async move {
                // A newer update journal must not be acknowledged by an old dialog.
                if let Ok(Some(mut current)) = Journal::load(&state_dir)
                    && current == journal
                {
                    current.stage = Stage::Acknowledged;
                    current.save(&state_dir).ok();
                }
            })
            .await;
    })
    .detach();
}

#[cfg(test)]
#[cfg(unix)]
mod tests {
    use super::*;
    use std::process::Command;

    #[test]
    fn update_relaunch_preserves_explicit_state_without_isolating_default_install() {
        let state = Path::new(r"C:\Users\Owen\AppData\Local\Krill");
        let default = Path::new("c:/users/owen/appdata/local/krill");
        for (path, default, explicit, sets_override) in [
            (state, Some(default), false, false),
            (state, Some(default), true, true),
            (Path::new("/isolated-state"), Some(default), false, true),
            (state, None, false, true),
        ] {
            let mut command = Command::new("fixture-not-executed");
            set_relaunch_state(&mut command, path, default, explicit);
            let override_value = command
                .get_envs()
                .find(|(key, _)| *key == "KRILL_STATE_DIR");
            assert_eq!(override_value.is_some(), sets_override);
            if let Some((_, value)) = override_value {
                assert_eq!(value, Some(path.as_os_str()));
            }
        }
    }

    #[test]
    fn regression_config_parse_error_aborts_relauncher() {
        let dir = std::env::temp_dir().join(format!("mk-relaunch-config-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join(super::super::CONFIG_FILE),
            "designated_requirement = [",
        )
        .unwrap();
        let args = [
            dir.join("missing.app").into_os_string(),
            dir.clone().into_os_string(),
            "0.1.0".into(),
            "0.2.0".into(),
            "10".into(),
        ];
        let result = run_relauncher_args(args.into_iter());
        fs::remove_dir_all(dir).unwrap();
        assert!(matches!(result, Err(Error::Config(_))), "{result:?}");
    }

    #[test]
    fn regression_swap_and_rollback_preserve_bundle_and_exe_inodes() {
        use std::os::unix::fs::MetadataExt as _;
        #[derive(Debug)]
        struct Accept;
        impl CodeVerifier for Accept {
            fn verify(&self, _: &Path) -> Result<(), Error> {
                Ok(())
            }
        }

        fn compile_signed_macho(out: &Path, exit_code: u8) {
            use std::io::Write as _;
            let mut child = Command::new("/usr/bin/clang")
                .args(["-x", "c", "-", "-o"])
                .arg(out)
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            child
                .stdin
                .as_mut()
                .unwrap()
                .write_all(format!("int main(void) {{ return {exit_code}; }}\n").as_bytes())
                .unwrap();
            let res = child.wait_with_output().unwrap();
            assert!(
                res.status.success(),
                "{}",
                String::from_utf8_lossy(&res.stderr)
            );
            let cs = Command::new("/usr/bin/codesign")
                .args(["--force", "--sign", "-"])
                .arg(out)
                .status()
                .unwrap();
            assert!(cs.success());
        }

        let dir = std::env::temp_dir().join(format!("mk-relaunch-inode-{}", std::process::id()));
        fs::remove_dir_all(&dir).ok();
        let app = dir.join("Krill.app");
        let staging = staging_path(&app);
        let state = dir.join("state");
        fs::create_dir_all(app.join("Contents/MacOS")).unwrap();
        fs::create_dir_all(staging.join("Contents/MacOS")).unwrap();
        fs::write(staging.join(super::super::BUNDLE_INFO), r#"<?xml version="1.0"?><plist version="1.0"><dict><key>CFBundleShortVersionString</key><string>0.2.0</string></dict></plist>"#).unwrap();
        let app_exe = app.join("Contents/MacOS/krill-app");
        let staging_exe = staging.join("Contents/MacOS/krill-app");
        compile_signed_macho(&app_exe, 11);
        compile_signed_macho(&staging_exe, 22);

        // Execute old Mach-O before swap to populate XNU UBC cs_blob cache
        assert_eq!(Command::new(&app_exe).status().unwrap().code(), Some(11));

        let bundle_ino = fs::metadata(&app).unwrap().ino();
        let macos_ino = fs::metadata(app.join("Contents/MacOS")).unwrap().ino();
        let exe_ino_before = fs::metadata(&app_exe).unwrap().ino();

        swap_bundles(
            &app,
            &state,
            "0.1.0",
            "0.2.0",
            Duration::from_secs(1),
            &Accept,
        )
        .unwrap();
        assert_eq!(fs::metadata(&app).unwrap().ino(), bundle_ino);
        assert_eq!(
            fs::metadata(app.join("Contents/MacOS")).unwrap().ino(),
            macos_ino
        );
        let exe_ino_after_swap = fs::metadata(&app_exe).unwrap().ino();
        assert_ne!(exe_ino_after_swap, exe_ino_before);
        // Must execute cleanly without Apple Silicon XNU cs_blob SIGKILL (-9)
        assert_eq!(Command::new(&app_exe).status().unwrap().code(), Some(22));

        rollback(&app, &state, "test rollback").unwrap();
        assert_eq!(fs::metadata(&app).unwrap().ino(), bundle_ino);
        assert_eq!(
            fs::metadata(app.join("Contents/MacOS")).unwrap().ino(),
            macos_ino
        );
        let exe_ino_after_rollback = fs::metadata(&app_exe).unwrap().ino();
        assert_ne!(exe_ino_after_rollback, exe_ino_after_swap);
        assert_eq!(Command::new(&app_exe).status().unwrap().code(), Some(11));

        fs::remove_dir_all(dir).ok();
    }
}
