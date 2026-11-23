//! Antigravity(`agy`) 단일 `~/.gemini` 네이티브 자격증명 교체. 다른 프로바이더는 기본 로그인을 건드리지 않는다.
//!
//! - Antigravity(`agy`): `--gemini_dir`로 `.gemini` 루트를 분리하지 않고 호스트 기본 `~/.gemini`를 그대로 쓰면서,
//!   계정 전환·실행 시 선택된 계정의 `antigravity-oauth-token`을 호스트 파일과 macOS 키체인(`gemini`/`antigravity`)에
//!   원자적으로 반영한다. 교체 전 호스트에서 갱신된 토큰은 동일 이메일의 프로필 슬롯에 먼저 되돌려 저장한다.
//! - Claude·Codex·`OpenCode`: 실행은 계정 폴더 env(`CLAUDE_CONFIG_DIR`·`CODEX_HOME`·`XDG_DATA_HOME`)로 고르고
//!   기본 로그인은 쓰지 않는다(Orca `ClaudeProfileRouter`·Codex 계정별 홈·`OpenCode` 데이터 계정과 같다).
//!   덮어쓰면 기본 로그인 토큰이 사라지고 기본 카드가 활성 계정 값을 보인다.
//! - 예전 Krill은 활성 Codex 토큰을 `~/.codex/auth.json`에 복사했다. 그때 기본 경로에서 갱신된 토큰은
//!   같은 신원의 프로필로 한 번만 옮긴다(Orca `legacy-shared-auth-migration`).

#[cfg(any(target_os = "macos", test))]
use std::io::Read as _;
#[cfg(any(target_os = "macos", test))]
use std::io::Write as _;
use std::path::Path;
#[cfg(any(target_os = "macos", test))]
use std::process::{Command, Stdio};
use std::sync::Mutex;
#[cfg(any(target_os = "macos", test))]
use std::time::{Duration, Instant};

use krill_proto::usage::ProviderKind;
use zeroize::Zeroizing;

use super::{AccountsFile, StoredAccount, identity_from_credentials, write_atomic_0600};

const MAX_TOKEN_BYTES: u64 = 64 * 1024;
#[cfg(target_os = "macos")]
const KEYCHAIN_TIMEOUT: Duration = Duration::from_secs(5);

/// `agy` 호스트 토큰 파일(`~/.gemini/antigravity-cli/antigravity-oauth-token`) 및
/// macOS 키체인 읽기·교체·삭제를 직렬화하는 전용 잠금. `SAVE_LOCK` 밖에서 잡는다.
static SYNC_LOCK: Mutex<()> = Mutex::new(());

fn active_dir_for(file: &AccountsFile, provider: ProviderKind) -> Option<&Path> {
    file.active
        .get(provider.as_str())
        .and_then(|active_id| {
            file.accounts.iter().find(|a| {
                a.id == *active_id
                    && ProviderKind::parse(&a.provider) == Some(provider)
                    && !a.profile_dir.is_empty()
            })
        })
        .map(|a| Path::new(&a.profile_dir))
}

/// `accounts.toml` 저장 시 활성 agy 계정의 자격증명을 호스트 기본 경로에 반영한다.
#[cfg(test)]
pub(crate) fn sync_active(home: &Path, file: &AccountsFile) {
    sync_account(
        home,
        &file.accounts,
        active_dir_for(file, ProviderKind::Antigravity),
    );
}

/// `SAVE_LOCK` 해제 뒤 최신 `accounts.toml`을 다시 읽어 활성 agy 계정을 호스트에 반영한다.
/// 동시 저장 경쟁이 있어도 직렬화 락 안에서 최신 디스크 상태를 읽어 역전 덮어쓰기를 막는다.
pub(crate) fn sync_active_from_disk(home: &Path, state_dir: Option<&Path>) {
    if home.as_os_str().is_empty() {
        return;
    }
    let host_token_path = home.join(".gemini/antigravity-cli/antigravity-oauth-token");
    crate::usage::direct::agy::run_account_op(|| {
        let _sync_guard = crate::session::lock(&SYNC_LOCK);
        let file = super::load_file(home, state_dir);
        let target_dir = active_dir_for(&file, ProviderKind::Antigravity);
        if target_dir.is_some_and(|dir| is_host_target(home, &host_token_path, dir)) {
            return;
        }
        sync_account_gated(home, &host_token_path, &file.accounts, target_dir);
    });
}

/// 명시된 Antigravity 계정 프로필 폴더(`dir`)의 자격증명을 호스트 `~/.gemini`로 활성화한다.
pub(crate) fn sync_account_dir(home: &Path, state_dir: Option<&Path>, dir: &Path) {
    sync_provider_account_dir(home, state_dir, ProviderKind::Antigravity, dir);
}

/// 명시된 프로바이더 계정 프로필 폴더(`dir`)의 자격증명을 호스트 기본 경로로 동기화한다.
pub(crate) fn sync_provider_account_dir(
    home: &Path,
    state_dir: Option<&Path>,
    provider: ProviderKind,
    dir: &Path,
) {
    if dir.as_os_str().is_empty() {
        return;
    }
    match provider {
        ProviderKind::Antigravity => {
            if home.as_os_str().is_empty() {
                return;
            }
            let host_token_path = home.join(".gemini/antigravity-cli/antigravity-oauth-token");
            if is_host_target(home, &host_token_path, dir) {
                return;
            }
            crate::usage::direct::agy::run_account_op(|| {
                let _sync_guard = crate::session::lock(&SYNC_LOCK);
                let file = super::load_file(home, state_dir);
                sync_account_gated(home, &host_token_path, &file.accounts, Some(dir));
            });
        }
        ProviderKind::Claude
        | ProviderKind::Codex
        | ProviderKind::OpenCode
        | ProviderKind::Gemini => {}
    }
}

/// 호스트 기본 경로(및 macOS 키체인)에서 갱신된 agy 토큰을 동일 신원의 프로필 슬롯으로 되돌린 뒤,
/// 같은 직렬화 락 안에서 후속 검사(`after`)를 실행한다.
pub(crate) fn reconcile_host_to_profiles_then<T>(
    home: &Path,
    accounts: &[StoredAccount],
    after: impl FnOnce() -> T,
) -> T {
    if home.as_os_str().is_empty() {
        return after();
    }
    let host_token_path = home.join(".gemini/antigravity-cli/antigravity-oauth-token");
    crate::usage::direct::agy::run_account_op(|| {
        let _sync_guard = crate::session::lock(&SYNC_LOCK);
        sync_account_gated(home, &host_token_path, accounts, None);
        after()
    })
}

fn is_host_target(home: &Path, host_token_path: &Path, dir: &Path) -> bool {
    dir == home || dir.join(".gemini/antigravity-cli/antigravity-oauth-token") == host_token_path
}

const CODEX_MIGRATION_MARKER: &str = "codex-host-auth-migrated-v1";

/// 예전 Krill이 `~/.codex/auth.json`에 복사한 활성 토큰이 거기서 갱신됐으면 같은 신원의 프로필로 한 번 옮긴다.
/// 계속 옮기면 같은 계정의 별도 로그인 둘이 한 refresh 토큰을 나눠 써서 한쪽이 로그아웃된다.
pub(crate) fn migrate_codex_host_once(
    home: &Path,
    state_dir: Option<&Path>,
    accounts: &[StoredAccount],
) {
    if !is_real_home(home) {
        return;
    }
    let marker = super::config_path(home, state_dir).with_file_name(CODEX_MIGRATION_MARKER);
    if marker.exists() {
        return;
    }
    preserve_codex_host(home, accounts);
    std::fs::write(&marker, b"").ok();
}

fn codex_account_id(raw: &[u8]) -> Option<String> {
    serde_json::from_slice::<serde_json::Value>(raw)
        .ok()?
        .pointer("/tokens/account_id")
        .and_then(serde_json::Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

fn preserve_codex_host(home: &Path, accounts: &[StoredAccount]) {
    let host_auth_path = home.join(".codex/auth.json");
    let Some(host_raw) = read_token_file(&host_auth_path) else {
        return;
    };
    let host_email = identity_from_credentials(ProviderKind::Codex, &host_raw).map(|i| i.email);
    let host_acc_id = codex_account_id(&host_raw);
    if host_email.is_none() && host_acc_id.is_none() {
        return;
    }
    for acc in accounts {
        if ProviderKind::parse(&acc.provider) != Some(ProviderKind::Codex)
            || acc.profile_dir.is_empty()
        {
            continue;
        }
        let prof_dir = Path::new(&acc.profile_dir);
        let prof_auth_path = prof_dir.join("auth.json");
        if prof_auth_path == host_auth_path {
            continue;
        }
        let Some(prof_raw) = read_token_file(&prof_auth_path) else {
            continue;
        };
        let prof_email = identity_from_credentials(ProviderKind::Codex, &prof_raw)
            .map(|i| i.email)
            .or_else(|| acc.email.clone());
        let same_identity = match (&host_email, &prof_email) {
            (Some(he), Some(pe)) => he.eq_ignore_ascii_case(pe),
            _ => host_acc_id.is_some() && host_acc_id == codex_account_id(&prof_raw),
        };
        if same_identity
            && prof_raw != host_raw
            && token_is_newer(
                ProviderKind::Codex,
                &host_raw,
                Some(&host_auth_path),
                &prof_raw,
                Some(&prof_auth_path),
                true,
            )
        {
            write_atomic_0600(&prof_auth_path, &host_raw).ok();
            crate::usage::direct::evict_profile(home, ProviderKind::Codex, prof_dir);
            super::login::evict_profile(ProviderKind::Codex, prof_dir);
        }
    }
}

#[cfg(test)]
fn sync_account(home: &Path, accounts: &[StoredAccount], target_dir: Option<&Path>) {
    if home.as_os_str().is_empty() {
        return;
    }
    let host_token_path = home.join(".gemini/antigravity-cli/antigravity-oauth-token");
    if target_dir.is_some_and(|dir| is_host_target(home, &host_token_path, dir)) {
        return;
    }
    crate::usage::direct::agy::run_account_op(|| {
        let _sync_guard = crate::session::lock(&SYNC_LOCK);
        sync_account_gated(home, &host_token_path, accounts, target_dir);
    });
}

fn sync_account_gated(
    home: &Path,
    host_token_path: &Path,
    accounts: &[StoredAccount],
    target_dir: Option<&Path>,
) {
    let real_keychain = use_real_keychain(home);
    let mut current_keychain = if real_keychain { read_keychain() } else { None };
    let mut current_file = read_token_file(host_token_path);

    // 1. 호스트에서 갱신된 토큰이 있으면 같은 이메일을 가진 프로필 슬롯에 먼저 보존한다.
    if current_file.is_some() || current_keychain.is_some() {
        for acc in accounts {
            if ProviderKind::parse(&acc.provider) != Some(ProviderKind::Antigravity)
                || acc.profile_dir.is_empty()
            {
                continue;
            }
            let prof_dir = Path::new(&acc.profile_dir);
            if !prof_dir.is_dir() {
                continue;
            }
            let prof_token_path = prof_dir.join(".gemini/antigravity-cli/antigravity-oauth-token");
            let Some(prof_raw) = read_token_file(&prof_token_path) else {
                continue;
            };
            let host_from_file = current_file.is_some();
            let Some(host_raw) = current_file.as_ref().or(current_keychain.as_ref()) else {
                break;
            };
            let Some(host_id) = identity_from_credentials(ProviderKind::Antigravity, host_raw)
            else {
                break;
            };
            let prof_email = identity_from_credentials(ProviderKind::Antigravity, &prof_raw)
                .map(|i| i.email)
                .or_else(|| acc.email.clone());
            let same_identity = prof_email
                .as_deref()
                .is_some_and(|e| e.eq_ignore_ascii_case(&host_id.email));
            if !same_identity || &prof_raw == host_raw {
                continue;
            }
            let is_target = target_dir == Some(prof_dir);
            let host_path_opt = host_from_file.then_some(host_token_path);
            if token_is_newer(
                ProviderKind::Antigravity,
                host_raw,
                host_path_opt,
                &prof_raw,
                Some(&prof_token_path),
                is_target,
            ) {
                write_atomic_0600(&prof_token_path, host_raw).ok();
                crate::usage::direct::evict_profile(home, ProviderKind::Antigravity, prof_dir);
                super::login::evict_profile(ProviderKind::Antigravity, prof_dir);
            } else if (target_dir.is_none() || is_target)
                && token_is_newer(
                    ProviderKind::Antigravity,
                    &prof_raw,
                    Some(&prof_token_path),
                    host_raw,
                    host_path_opt,
                    false,
                )
            {
                write_atomic_0600(host_token_path, &prof_raw).ok();
                if real_keychain {
                    write_keychain(&prof_raw);
                    current_keychain = Some(prof_raw.clone());
                }
                current_file = Some(prof_raw);
                let host_dir = super::host_default_dir(home, ProviderKind::Antigravity);
                crate::usage::direct::evict_profile(home, ProviderKind::Antigravity, &host_dir);
                super::login::evict_profile(ProviderKind::Antigravity, &host_dir);
            }
        }
    }

    // 2. 선택된 프로필의 토큰을 호스트 파일 및 macOS 키체인에 반영한다.
    //    대상 프로필이 미로그인 상태면 이전 계정의 호스트 토큰·키체인을 정리해 오실행을 막는다.
    let Some(target) = target_dir.filter(|d| !d.as_os_str().is_empty()) else {
        return;
    };
    let target_token_path = target.join(".gemini/antigravity-cli/antigravity-oauth-token");
    let mut host_changed = false;
    if let Some(target_raw) = read_token_file(&target_token_path) {
        if current_file.as_ref() != Some(&target_raw) {
            write_atomic_0600(host_token_path, &target_raw).ok();
            host_changed = true;
        }
        if real_keychain && current_keychain.as_ref() != Some(&target_raw) {
            write_keychain(&target_raw);
            host_changed = true;
        }
    } else {
        if std::fs::symlink_metadata(host_token_path).is_ok()
            && std::fs::remove_file(host_token_path).is_ok()
        {
            host_changed = true;
        }
        if real_keychain && current_keychain.is_some() && delete_keychain() {
            host_changed = true;
        }
    }
    if host_changed {
        let host_dir = super::host_default_dir(home, ProviderKind::Antigravity);
        crate::usage::direct::evict_profile(home, ProviderKind::Antigravity, &host_dir);
        super::login::evict_profile(ProviderKind::Antigravity, &host_dir);
    }
}

fn is_real_home(home: &Path) -> bool {
    if cfg!(test) {
        return false;
    }
    if std::env::var_os("KRILL_TEST_OFFLINE").is_some_and(|v| !v.is_empty() && v != "0") {
        return false;
    }
    let Some(real_home) = krill_proto::os::home_dir() else {
        return false;
    };
    home == real_home
        || home
            .canonicalize()
            .ok()
            .zip(real_home.canonicalize().ok())
            .is_some_and(|(a, b)| a == b)
}

fn use_real_keychain(home: &Path) -> bool {
    #[cfg(target_os = "macos")]
    {
        is_real_home(home)
            && !home
                .join(".gemini/antigravity-cli/cache/antigravity-keyring-unavailable")
                .exists()
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = home;
        false
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct TokenStamp {
    issued_ns: i128,
    expiry_ns: i128,
}

fn now_ns() -> i128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|d| i128::try_from(d.as_nanos()).ok())
        .unwrap_or(0)
}

fn rfc3339_ns(s: &str) -> Option<i128> {
    let secs = i128::from(krill_proto::time::parse_rfc3339_epoch(s)?);
    let (_, rest) = s.split_once(['T', 't', ' '])?;
    let clock = rest
        .strip_suffix(['Z', 'z'])
        .or_else(|| rest.rsplit_once(['+', '-']).map(|(clock, _)| clock))?;
    let nanos = match clock.split_once(['.', ',']) {
        None => 0_i128,
        Some((_, frac)) => {
            let mut acc = 0_i128;
            let mut scale = 100_000_000_i128;
            for b in frac.bytes().take(9) {
                if !b.is_ascii_digit() {
                    return None;
                }
                acc += i128::from(b - b'0') * scale;
                scale /= 10;
            }
            acc
        }
    };
    secs.checked_mul(1_000_000_000)?.checked_add(nanos)
}

fn jwt_times_ns(jwt: &str) -> (Option<i128>, Option<i128>) {
    let Some(claims) = krill_proto::account_login::jwt_claims(jwt) else {
        return (None, None);
    };
    let ts = |key: &str| {
        claims
            .get(key)
            .and_then(serde_json::Value::as_i64)
            .and_then(|v| i128::from(v).checked_mul(1_000_000_000))
    };
    (ts("iat"), ts("exp"))
}

fn codex_token_stamp(raw: &[u8]) -> Option<TokenStamp> {
    let v: serde_json::Value = serde_json::from_slice(raw).ok()?;
    let last_refresh = v
        .get("last_refresh")
        .and_then(serde_json::Value::as_str)
        .and_then(rfc3339_ns);
    let (acc_iat, acc_exp) = v
        .pointer("/tokens/access_token")
        .and_then(serde_json::Value::as_str)
        .map_or((None, None), jwt_times_ns);
    let (id_iat, id_exp) = v
        .pointer("/tokens/id_token")
        .or_else(|| v.pointer("/tokens/idToken"))
        .and_then(serde_json::Value::as_str)
        .map_or((None, None), jwt_times_ns);
    let issued = [last_refresh, acc_iat, id_iat].into_iter().flatten().max();
    let expiry = [acc_exp, id_exp].into_iter().flatten().max();
    if issued.is_none() && expiry.is_none() {
        return None;
    }
    Some(TokenStamp {
        issued_ns: issued.unwrap_or(i128::MIN),
        expiry_ns: expiry.unwrap_or(i128::MIN),
    })
}

fn agy_token_stamp(raw: &[u8]) -> Option<TokenStamp> {
    let v: serde_json::Value = serde_json::from_slice(raw).ok()?;
    let expiry_rfc = v
        .pointer("/token/expiry")
        .or_else(|| v.get("expiry"))
        .and_then(serde_json::Value::as_str)
        .and_then(rfc3339_ns);
    let (id_iat, id_exp) = v
        .pointer("/token/id_token")
        .or_else(|| v.get("id_token"))
        .and_then(serde_json::Value::as_str)
        .map_or((None, None), jwt_times_ns);
    let issued = id_iat;
    let expiry = [expiry_rfc, id_exp].into_iter().flatten().max();
    if issued.is_none() && expiry.is_none() {
        return None;
    }
    Some(TokenStamp {
        issued_ns: issued.unwrap_or(i128::MIN),
        expiry_ns: expiry.unwrap_or(i128::MIN),
    })
}

fn token_is_newer(
    provider: ProviderKind,
    cand_raw: &[u8],
    cand_path: Option<&Path>,
    curr_raw: &[u8],
    curr_path: Option<&Path>,
    strict_mtime: bool,
) -> bool {
    let stamp = match provider {
        ProviderKind::Codex => codex_token_stamp,
        ProviderKind::Antigravity => agy_token_stamp,
        _ => return false,
    };
    match (stamp(cand_raw), stamp(curr_raw)) {
        (Some(cs), Some(us)) => cs > us,
        (Some(cs), None) => cs.expiry_ns == i128::MIN || cs.expiry_ns > now_ns(),
        (None, Some(us)) => us.expiry_ns != i128::MIN && us.expiry_ns <= now_ns(),
        (None, None) => {
            let mtime = |p: Option<&Path>| std::fs::metadata(p?).ok()?.modified().ok();
            match (mtime(cand_path), mtime(curr_path)) {
                (Some(cm), Some(um)) => {
                    if strict_mtime {
                        cm > um
                    } else {
                        cm >= um
                    }
                }
                _ => !strict_mtime,
            }
        }
    }
}

fn read_token_file(path: &Path) -> Option<Zeroizing<Vec<u8>>> {
    let meta = std::fs::symlink_metadata(path).ok()?;
    if !meta.is_file()
        || meta.file_type().is_symlink()
        || meta.len() == 0
        || meta.len() > MAX_TOKEN_BYTES
    {
        return None;
    }
    let bytes = Zeroizing::new(std::fs::read(path).ok()?);
    valid_token(&bytes).then_some(bytes)
}

fn valid_token(raw: &[u8]) -> bool {
    !raw.is_empty()
        && u64::try_from(raw.len()).is_ok_and(|len| len <= MAX_TOKEN_BYTES)
        && serde_json::from_slice::<serde_json::Value>(raw).is_ok_and(|v| v.is_object())
}

#[cfg(any(target_os = "macos", test))]
fn run_command_with_timeout(
    cmd: &mut Command,
    stdin_bytes: Option<&[u8]>,
    capture_stdout: bool,
    max_stdout: u64,
    timeout: Duration,
) -> Option<(std::process::ExitStatus, Vec<u8>)> {
    cmd.stdin(if stdin_bytes.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    })
    .stdout(if capture_stdout {
        Stdio::piped()
    } else {
        Stdio::null()
    })
    .stderr(Stdio::null());
    let mut child = cmd.spawn().ok()?;
    let reader = if capture_stdout {
        let stdout = child.stdout.take()?;
        let handle = std::thread::Builder::new()
            .name("agy-keychain-read".into())
            .spawn(move || {
                let mut buf = Vec::new();
                let limit = max_stdout.saturating_add(1);
                stdout.take(limit).read_to_end(&mut buf).ok()?;
                (u64::try_from(buf.len()).ok()? <= max_stdout).then_some(buf)
            })
            .ok();
        let Some(handle) = handle else {
            child.kill().ok();
            child.wait().ok();
            return None;
        };
        Some(handle)
    } else {
        None
    };
    if let Some(bytes) = stdin_bytes {
        let wrote = child
            .stdin
            .take()
            .is_some_and(|mut stdin| stdin.write_all(bytes).is_ok());
        if !wrote {
            child.kill().ok();
            child.wait().ok();
            if let Some(handle) = reader {
                drop(handle.join());
            }
            return None;
        }
    }
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if Instant::now() >= deadline => {
                child.kill().ok();
                child.wait().ok();
                break None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            Err(_) => {
                child.kill().ok();
                child.wait().ok();
                break None;
            }
        }
    };
    let stdout = match reader {
        Some(handle) => handle.join().ok().flatten(),
        None => Some(Vec::new()),
    };
    let status = status?;
    let stdout = stdout?;
    Some((status, stdout))
}

#[cfg(target_os = "macos")]
fn read_keychain() -> Option<Zeroizing<Vec<u8>>> {
    let mut cmd = Command::new("/usr/bin/security");
    cmd.args([
        "find-generic-password",
        "-s",
        "gemini",
        "-a",
        "antigravity",
        "-w",
    ]);
    let (status, stdout) =
        run_command_with_timeout(&mut cmd, None, true, MAX_TOKEN_BYTES, KEYCHAIN_TIMEOUT)?;
    if !status.success() || u64::try_from(stdout.len()).ok()? > MAX_TOKEN_BYTES {
        return None;
    }
    let text = std::str::from_utf8(&stdout).ok()?.trim();
    let decoded = if let Some(b64) = text.strip_prefix("go-keyring-base64:") {
        Zeroizing::new(base64_decode(b64)?)
    } else if let Some(hex) = text.strip_prefix("go-keyring-encoded:") {
        let (pairs, rem) = hex.as_bytes().as_chunks::<2>();
        if !rem.is_empty() {
            return None;
        }
        let bytes = pairs
            .iter()
            .map(|p| u8::from_str_radix(std::str::from_utf8(p).ok()?, 16).ok())
            .collect::<Option<Vec<_>>>()?;
        Zeroizing::new(bytes)
    } else {
        Zeroizing::new(text.as_bytes().to_vec())
    };
    valid_token(&decoded).then_some(decoded)
}

#[cfg(not(target_os = "macos"))]
fn read_keychain() -> Option<Zeroizing<Vec<u8>>> {
    None
}

#[cfg(target_os = "macos")]
fn write_keychain(raw: &[u8]) -> bool {
    let encoded = format!("go-keyring-base64:{}", base64_encode(raw));
    let line = Zeroizing::new(format!(
        "add-generic-password -U -s \"gemini\" -a \"antigravity\" -w {encoded}\n"
    ));
    if line.len() > 16 * 1024 {
        return false;
    }
    let mut cmd = Command::new("/usr/bin/security");
    cmd.arg("-i");
    run_command_with_timeout(&mut cmd, Some(line.as_bytes()), false, 0, KEYCHAIN_TIMEOUT)
        .is_some_and(|(status, _)| status.success())
}

#[cfg(not(target_os = "macos"))]
fn write_keychain(_raw: &[u8]) -> bool {
    false
}

#[cfg(target_os = "macos")]
fn delete_keychain() -> bool {
    let mut cmd = Command::new("/usr/bin/security");
    cmd.args([
        "delete-generic-password",
        "-s",
        "gemini",
        "-a",
        "antigravity",
    ]);
    run_command_with_timeout(&mut cmd, None, false, 0, KEYCHAIN_TIMEOUT)
        .is_some_and(|(status, _)| status.success())
}

#[cfg(not(target_os = "macos"))]
fn delete_keychain() -> bool {
    false
}

#[cfg(target_os = "macos")]
fn base64_encode(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let char_at = |idx: u8| char::from(TABLE.get(usize::from(idx & 0x3f)).copied().unwrap_or(b'A'));
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let Some(&b0) = chunk.first() else {
            continue;
        };
        let b1 = chunk.get(1).copied().unwrap_or(0);
        let b2 = chunk.get(2).copied().unwrap_or(0);
        out.push(char_at(b0 >> 2));
        out.push(char_at(((b0 & 0x03) << 4) | (b1 >> 4)));
        if chunk.len() > 1 {
            out.push(char_at(((b1 & 0x0f) << 2) | (b2 >> 6)));
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(char_at(b2 & 0x3f));
        } else {
            out.push('=');
        }
    }
    out
}

#[cfg(target_os = "macos")]
fn base64_decode(input: &str) -> Option<Vec<u8>> {
    let trimmed = input.trim().trim_end_matches('=');
    let mut decoded = Vec::with_capacity(trimmed.len() * 3 / 4);
    let (mut bits, mut count) = (0_u32, 0_u32);
    for byte in trimmed.bytes().filter(|b| !b.is_ascii_whitespace()) {
        let n = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            _ => return None,
        };
        bits = (bits << 6) | u32::from(n);
        count += 6;
        if count >= 8 {
            count -= 8;
            decoded.push(u8::try_from((bits >> count) & 255).ok()?);
        }
    }
    if count >= 6 || bits & ((1 << count) - 1) != 0 {
        return None;
    }
    Some(decoded)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::usage::accounts::{AccountsFile, StoredAccount};

    #[test]
    fn switching_agy_account_swaps_host_token_and_preserves_refreshed_host_token() {
        let root =
            std::env::temp_dir().join(format!("krill-agy-native-swap-{}", std::process::id()));
        std::fs::remove_dir_all(&root).ok();
        let home = root.join("home");
        let prof1 = root.join("prof1");
        let prof2 = root.join("prof2");
        for dir in [&home, &prof1, &prof2] {
            std::fs::create_dir_all(dir.join(".gemini/antigravity-cli")).unwrap();
        }
        let tok1_initial = br#"{"auth_method":"consumer","id_token_email":"one@example.test","token":{"access_token":"tok1-old"}}"#;
        let tok1_refreshed = br#"{"auth_method":"consumer","id_token_email":"one@example.test","token":{"access_token":"tok1-refreshed"}}"#;
        let tok2 = br#"{"auth_method":"consumer","id_token_email":"two@example.test","token":{"access_token":"tok2"}}"#;
        std::fs::write(
            prof1.join(".gemini/antigravity-cli/antigravity-oauth-token"),
            tok1_initial,
        )
        .unwrap();
        std::fs::write(
            prof2.join(".gemini/antigravity-cli/antigravity-oauth-token"),
            tok2,
        )
        .unwrap();

        let mut file = AccountsFile {
            accounts: vec![
                StoredAccount {
                    id: "agy-1".into(),
                    provider: "agy".into(),
                    label: "One".into(),
                    email: Some("one@example.test".into()),
                    profile_dir: prof1.display().to_string(),
                },
                StoredAccount {
                    id: "agy-2".into(),
                    provider: "agy".into(),
                    label: "Two".into(),
                    email: Some("two@example.test".into()),
                    profile_dir: prof2.display().to_string(),
                },
            ],
            ..AccountsFile::default()
        };

        // 1. agy-1 활성화 시 호스트 토큰이 agy-1 토큰으로 채워진다.
        file.active.insert("agy".into(), "agy-1".into());
        sync_active(&home, &file);
        assert_eq!(
            std::fs::read(home.join(".gemini/antigravity-cli/antigravity-oauth-token")).unwrap(),
            tok1_initial
        );

        // 2. 호스트에서 agy가 토큰을 갱신한 뒤 agy-2로 전환하면, 갱신된 토큰이 prof1에 보존되고
        // 호스트는 agy-2가 되며 이전 계정의 호스트 미터 캐시도 즉시 비워진다.
        let host_dir = crate::usage::accounts::host_default_dir(&home, ProviderKind::Antigravity);
        crate::usage::meters::remember(
            &home,
            ProviderKind::Antigravity,
            &host_dir,
            krill_proto::usage::AccountMeters {
                account_id: "agy-host".into(),
                plan: None,
                meters: vec![krill_proto::usage::UsageMeter {
                    key: "gemini_weekly".into(),
                    label: "Gemini · 7d".into(),
                    value: None,
                    window: None,
                }],
            },
        );
        assert!(crate::usage::meters::has_cached(
            &home,
            ProviderKind::Antigravity,
            &host_dir
        ));
        std::fs::write(
            home.join(".gemini/antigravity-cli/antigravity-oauth-token"),
            tok1_refreshed,
        )
        .unwrap();
        file.active.insert("agy".into(), "agy-2".into());
        sync_active(&home, &file);
        assert!(!crate::usage::meters::has_cached(
            &home,
            ProviderKind::Antigravity,
            &host_dir
        ));
        assert_eq!(
            std::fs::read(prof1.join(".gemini/antigravity-cli/antigravity-oauth-token")).unwrap(),
            tok1_refreshed
        );
        assert_eq!(
            std::fs::read(home.join(".gemini/antigravity-cli/antigravity-oauth-token")).unwrap(),
            tok2
        );

        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn switching_claude_codex_and_opencode_accounts_keeps_the_default_login() {
        let root =
            std::env::temp_dir().join(format!("krill-default-login-kept-{}", std::process::id()));
        std::fs::remove_dir_all(&root).ok();
        let home = root.join("home");
        let mut cases = Vec::new();
        for (provider, host, profile) in [
            (
                ProviderKind::Claude,
                ".claude/.credentials.json",
                ".credentials.json",
            ),
            (ProviderKind::Codex, ".codex/auth.json", "auth.json"),
            (
                ProviderKind::OpenCode,
                ".local/share/opencode/auth.json",
                "data/opencode/auth.json",
            ),
        ] {
            let dir = root.join(provider.as_str());
            let (host, profile) = (home.join(host), dir.join(profile));
            for path in [&host, &profile] {
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            }
            std::fs::write(&host, br#"{"default":"login"}"#).unwrap();
            std::fs::write(&profile, br#"{"profile":"login"}"#).unwrap();
            cases.push((provider, dir, host));
        }
        let mut file = AccountsFile::default();
        for (provider, dir, host) in &cases {
            sync_provider_account_dir(&home, None, *provider, dir);
            file.accounts.push(StoredAccount {
                id: provider.as_str().into(),
                provider: provider.as_str().into(),
                label: provider.as_str().into(),
                email: None,
                profile_dir: dir.display().to_string(),
            });
            file.active
                .insert(provider.as_str().into(), provider.as_str().into());
            assert_eq!(std::fs::read(host).unwrap(), br#"{"default":"login"}"#);
        }
        sync_active(&home, &file);
        for (_, _, host) in &cases {
            assert_eq!(std::fs::read(host).unwrap(), br#"{"default":"login"}"#);
        }
        std::fs::remove_dir_all(root).ok();
    }

    fn encode_b64url(bytes: &[u8]) -> String {
        const TABLE: &[u8; 64] =
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
        let (mut bits, mut count) = (0_u32, 0_u32);
        let mut out = String::new();
        for &byte in bytes {
            bits = (bits << 8) | u32::from(byte);
            count += 8;
            while count >= 6 {
                count -= 6;
                let idx = usize::try_from((bits >> count) & 63).unwrap();
                out.push(char::from(*TABLE.get(idx).unwrap()));
            }
        }
        if count > 0 {
            let idx = usize::try_from((bits << (6 - count)) & 63).unwrap();
            out.push(char::from(*TABLE.get(idx).unwrap()));
        }
        out
    }

    fn codex_jwt(payload: &serde_json::Value) -> String {
        let encoded = encode_b64url(&serde_json::to_vec(payload).unwrap());
        format!("eyJhbGciOiJub25lIn0.{encoded}.sig")
    }

    fn codex_auth_payload(
        last_refresh: &str,
        access_iat: i64,
        access_exp: i64,
        id_iat: i64,
        id_exp: i64,
    ) -> String {
        serde_json::json!({
            "last_refresh": last_refresh,
            "tokens": {
                "access_token": codex_jwt(&serde_json::json!({
                    "iat": access_iat,
                    "exp": access_exp,
                })),
                "id_token": codex_jwt(&serde_json::json!({
                    "email": "alpha@example.test",
                    "iat": id_iat,
                    "exp": id_exp,
                })),
                "account_id": "acc-relogin",
            }
        })
        .to_string()
    }

    #[test]
    fn codex_migration_moves_only_a_newer_same_identity_default_token_and_never_writes_default() {
        let root =
            std::env::temp_dir().join(format!("krill-codex-host-migration-{}", std::process::id()));
        std::fs::remove_dir_all(&root).ok();
        let home = root.join("home");
        let prof = root.join("prof");
        std::fs::create_dir_all(home.join(".codex")).unwrap();
        std::fs::create_dir_all(&prof).unwrap();
        let older = codex_auth_payload(
            "2026-10-28T11:02:37Z",
            1_793_185_356,
            1_794_049_356,
            1_793_185_356,
            1_793_188_956,
        );
        let newer = codex_auth_payload(
            "2026-11-08T14:24:24Z",
            1_794_147_864,
            1_795_011_864,
            1_794_147_864,
            1_794_151_464,
        );
        let accounts = vec![StoredAccount {
            id: "codex-alpha".into(),
            provider: "codex".into(),
            label: "alpha@example.test".into(),
            email: Some("alpha@example.test".into()),
            profile_dir: prof.display().to_string(),
        }];
        let read = |path: &Path| std::fs::read(path).unwrap();

        // A profile relogin newer than the default copy stays, and the default is never written.
        std::fs::write(prof.join("auth.json"), &newer).unwrap();
        std::fs::write(home.join(".codex/auth.json"), &older).unwrap();
        preserve_codex_host(&home, &accounts);
        assert_eq!(read(&prof.join("auth.json")), newer.as_bytes());
        assert_eq!(read(&home.join(".codex/auth.json")), older.as_bytes());

        // A default copy refreshed after the old swap moves back to its profile.
        std::fs::write(prof.join("auth.json"), &older).unwrap();
        std::fs::write(home.join(".codex/auth.json"), &newer).unwrap();
        preserve_codex_host(&home, &accounts);
        assert_eq!(read(&prof.join("auth.json")), newer.as_bytes());

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn switching_to_unauthenticated_agy_profile_clears_host_token_and_preserves_previous() {
        let root = std::env::temp_dir().join(format!(
            "krill-agy-native-unauth-clear-{}",
            std::process::id()
        ));
        std::fs::remove_dir_all(&root).ok();
        let home = root.join("home");
        let state = root.join("state");
        let prof1 = root.join("prof1");
        let prof2 = root.join("prof2");
        for dir in [&home, &prof1, &prof2] {
            std::fs::create_dir_all(dir.join(".gemini/antigravity-cli")).unwrap();
        }
        std::fs::create_dir_all(&state).unwrap();
        let tok1_old = br#"{"auth_method":"consumer","id_token_email":"one@example.test","token":{"access_token":"tok1-old"}}"#;
        let tok1_refreshed = br#"{"auth_method":"consumer","id_token_email":"one@example.test","token":{"access_token":"tok1-refreshed"}}"#;
        std::fs::write(
            prof1.join(".gemini/antigravity-cli/antigravity-oauth-token"),
            tok1_old,
        )
        .unwrap();
        let host_tok = home.join(".gemini/antigravity-cli/antigravity-oauth-token");
        std::fs::write(&host_tok, tok1_refreshed).unwrap();

        let mut file = AccountsFile {
            accounts: vec![
                StoredAccount {
                    id: "agy-1".into(),
                    provider: "agy".into(),
                    label: "One".into(),
                    email: Some("one@example.test".into()),
                    profile_dir: prof1.display().to_string(),
                },
                StoredAccount {
                    id: "agy-2".into(),
                    provider: "agy".into(),
                    label: "Two".into(),
                    email: None,
                    profile_dir: prof2.display().to_string(),
                },
            ],
            ..AccountsFile::default()
        };
        file.active.insert("agy".into(), "agy-2".into());
        super::super::save_file(&home, Some(&state), &file).unwrap();

        assert_eq!(
            std::fs::read(prof1.join(".gemini/antigravity-cli/antigravity-oauth-token")).unwrap(),
            tok1_refreshed,
            "previous account's refreshed host token must be backed up before clearing"
        );
        assert!(
            !host_tok.exists(),
            "switching to an unauthenticated agy profile must clear the previous host token"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    #[cfg(unix)]
    fn keychain_command_timeout_kills_hanging_process() {
        let mut cmd = Command::new("/bin/sh");
        cmd.args(["-c", "sleep 30"]);
        let started = Instant::now();
        let out = run_command_with_timeout(&mut cmd, None, true, 1024, Duration::from_millis(80));
        assert!(out.is_none());
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "hanging helper must be killed promptly on timeout"
        );
    }
}
