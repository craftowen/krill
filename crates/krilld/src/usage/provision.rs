//! 계정 프로필 프로비저닝(OWE-699, MK-139). `accounts.toml`의 `provisioning`이 켜졌을 때만 동작한다(키가 없으면 켜짐).
//!
//! 구조는 Orca `claude-profile-provisioning.ts`·`claude-profile-history.ts`(MIT)를 참고했다(코드 복사 없음).
//! 참고 출처: Orca, Copyright (c) 2026 Lovecast Inc. (MIT), `.ref/orca/src/main/claude-accounts/`.
//! Claude 기록은 `~/.claude`로 모은다. 기존 프로필 기록과 충돌 사본은 백업에 보존한다.
//! 로그인 정보는 프로필에 남기고 MCP의 없는 서버 정의만 병합한다.
//! 멱등이라 계정 추가 때와 실행 전마다 다시 불러도 된다.

use std::io;
use std::path::Path;

use krill_proto::usage::ProviderKind;

mod agy;
#[cfg(unix)]
mod claude_history;
mod codex;
#[cfg(unix)]
mod history_jobs;
#[cfg(unix)]
mod history_probe;
#[cfg(unix)]
mod history_store;

// 오웬 승인(OWE-720): 대화 기록 공유, settings.json 최초 한 번 복사, 프로필 기존 내용 보존.
// Orca CLAUDE_PROFILE_HISTORY_DIRS와의 항목별 비교: 비공개 OWE-720 프로비저닝 비교 기록.

/// 프로필에 `~/.claude/<이름>`을 가리키는 심볼릭 링크로 공유하는 폴더(대화 기록 + 사용자 확장).
/// Unix에서는 원본이 없어도 만들어 첫 실행부터 기록 위치가 갈라지지 않게 한다.
pub const LINKED: &[&str] = &[
    "projects",        // 세션 JSONL: --resume/-c 대상 대화.
    "session-env",     // 세션별 환경 복원 자료(Orca 기록 공유 목록).
    "file-history",    // 세션 파일 변경 이력.
    "shell-snapshots", // 재개할 세션이 참조하는 셸 환경 스냅샷.
    "todos",           // 세션별 할 일 상태.
    "paste-cache",     // 대화가 참조하는 긴 붙여넣기 내용.
    "tasks",           // 세션 작업 목록·진행 상태.
    "plans",           // 대화가 참조하는 계획 파일.
    "transcripts",     // 대화 전사 자료(Orca 기록 공유 목록).
    "skills",
    "commands",
    "agents",
    "plugins",
];
/// 프로필에 없을 때만 한 번 복사하는 파일. 복사 뒤 프로필 쪽 수정은 보존된다(덮어쓰지 않음).
pub const COPIED: &[&str] = &["settings.json"];
/// 링크·통째 복사를 금지하는 로그인·런타임 상태. 위 두 목록과 겹치면 안 된다.
/// `.claude.json`은 `merge_claude_mcp`의 mcpServers 허용 목록 병합만 예외다.
pub const EXCLUDED: &[&str] = &[
    ".credentials.json",
    ".claude.json",
    ".claude.json.backup",
    "sessions", // 폴더 데몬에 귀속된 실행 중 프로세스 레지스트리(Orca도 공유 해제).
    "statsig",
    "ide",
    "cache",
    "debug",
    "usage_cache.json",
];
/// 프로필 `CLAUDE.md`가 없을 때 쓰는 내용: 사용자 전역 메모리를 import한다(사본이 아니라 원본을 따라간다).
pub const MEMORY_IMPORT: &str = "@~/.claude/CLAUDE.md\n";

/// ponytail: 프로세스 전역 리프 잠금(다른 잠금 안에서 잡지 않는다). 같은 새 계정을 동시에 띄워도
/// 임시 파일·링크가 엇갈리지 않는다. 처리량이 문제면 프로필별 잠금으로.
static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// 한 번의 프로비저닝 결과. 항목 이름만 담는다(경로는 로그에 남기지 않는다, CONVENTIONS A8).
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Report {
    /// 이번에 새로 만든 링크·사본.
    pub changed: Vec<&'static str>,
    /// 프로필에 다른 내용이 이미 있어 그대로 둔 항목.
    pub conflicts: Vec<&'static str>,
}

/// `accounts.toml`이 켜져 있을 때만 등록 프로필을 프로비저닝한다. 실패는 경고만 남긴다.
/// 호스트 기본 계정(`profile_dir` 빈 값)은 건드리지 않는다. agy는 네이티브 인증 미지원이다.
pub fn provision_if_enabled(
    home: &Path,
    state_dir: Option<&Path>,
    provider: ProviderKind,
    profile_dir: &str,
) {
    if profile_dir.is_empty() {
        return;
    }
    let file = super::accounts::load_file(home, state_dir);
    // 등록된 동일 프로바이더 계정 폴더만 프로필이다. 데몬 env에서 물려받은 실제 `CLAUDE_CONFIG_DIR`처럼
    // 사용자 자신의 설정 폴더가 실행 계정으로 들어와도 건드리지 않는다.
    let registered = file.accounts.iter().any(|a| {
        ProviderKind::parse(&a.provider) == Some(provider)
            && a.config_dir() == Path::new(profile_dir)
    });
    if !file.provisioning || !registered {
        return;
    }
    #[cfg(unix)]
    if match provider {
        ProviderKind::Claude => {
            claude_history::needs_merge(&home.join(".claude"), Path::new(profile_dir))
        }
        ProviderKind::Codex => codex::needs_merge(home, Path::new(profile_dir)),
        ProviderKind::Antigravity => agy::needs_merge(home, Path::new(profile_dir)),
        _ => false,
    } {
        let home = home.to_path_buf();
        let state = state_dir.map(Path::to_path_buf);
        let profile = profile_dir.to_owned();
        history_jobs::spawn(Path::new(profile_dir), move || {
            let current = super::accounts::load_file(&home, state.as_deref());
            if current.provisioning
                && current.accounts.iter().any(|account| {
                    ProviderKind::parse(&account.provider) == Some(provider)
                        && account.config_dir() == Path::new(&profile)
                })
            {
                provision_now(&home, provider, &profile);
            }
        });
        return;
    }
    provision_now(home, provider, profile_dir);
}

fn provision_now(home: &Path, provider: ProviderKind, profile_dir: &str) {
    let _guard = LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let result = match provider {
        ProviderKind::Claude => provision_claude(home, Path::new(profile_dir)),
        ProviderKind::Codex => codex::provision(home, Path::new(profile_dir)),
        ProviderKind::Antigravity => agy::provision(home, Path::new(profile_dir)),
        _ => return,
    };
    match result {
        Ok(report) if !report.conflicts.is_empty() => {
            tracing::warn!(conflicts = ?report.conflicts, "profile provisioning left existing entries");
        }
        Ok(_) => {}
        Err(error)
            if provider == ProviderKind::Antigravity
                && error.kind() == io::ErrorKind::Unsupported =>
        {
            // OWE-901: agy 미지원은 예상된 보류 상태라 debug로 둔다(매 실행 warn 스팸 방지).
            tracing::debug!(kind = ?error.kind(), "agy native account switching is unsupported; owner decision required");
        }
        Err(error) => {
            tracing::warn!(provider = provider.as_str(), kind = ?error.kind(), "profile provisioning failed");
        }
    }
}

/// `profile`을 사용자 `~/.claude` 공유 프로필로 맞춘다. `profile`이 `~/.claude` 자신이거나 그 안·조상이면 거부한다.
pub fn provision_claude(home: &Path, profile: &Path) -> io::Result<Report> {
    #[cfg(unix)]
    let idle = || claude_history::quiescent(profile);
    #[cfg(not(unix))]
    let idle = || false;
    provision_claude_inner(home, profile, &idle)
}

#[cfg_attr(
    not(unix),
    expect(unused_variables, reason = "기록 이행 프로브는 Unix에서만 사용한다")
)]
fn provision_claude_inner(
    home: &Path,
    profile: &Path,
    idle: &dyn Fn() -> bool,
) -> io::Result<Report> {
    let host = super::accounts::host_default_dir(home, ProviderKind::Claude);
    let mut report = Report::default();
    let profile_real = profile.canonicalize()?;
    let host_real = match host.canonicalize() {
        Ok(path) => path,
        // ~/.claude가 없어도 ~/.claude.json의 MCP는 공유할 수 있다. 원본 폴더는 만들지 않는다.
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            home.canonicalize()?.join(".claude")
        }
        Err(error) => return Err(error),
    };
    // 원본 안(Orca 감지 경로 등)이거나 원본의 조상(홈 자체 등)이면 거부한다.
    if profile_real.starts_with(&host_real) || host_real.starts_with(&profile_real) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "profile overlaps the host claude folder",
        ));
    }
    #[cfg(unix)]
    claude_history::share(&host, &profile_real, idle, &mut report)?;
    #[cfg(not(unix))]
    for &name in LINKED {
        let src = host.join(name);
        if src.is_dir() {
            link(&src, &profile.join(name), name, &mut report)?;
        }
    }
    for &name in COPIED {
        let src = host.join(name);
        if src.is_file() {
            let dst = profile.join(name);
            publish(
                &dst,
                |tmp| std::fs::copy(&src, tmp).map(drop),
                name,
                &mut report,
            )?;
        }
    }
    if host.join("CLAUDE.md").is_file() {
        publish(
            &profile.join("CLAUDE.md"),
            |tmp| std::fs::write(tmp, MEMORY_IMPORT),
            "CLAUDE.md",
            &mut report,
        )?;
    }
    merge_claude_mcp(home, &profile_real, &mut report)?;
    Ok(report)
}

/// 허용 목록은 mcpServers뿐이다. Orca claude-accounts/claude-profile-provisioning.ts의
/// 계정 종속 키(oauthAccount, userID, machineID, primaryApiKey, *Cache 등)와
/// Claude의 claudeAiOauth·mcpOAuth 토큰 저장소는 이 경로로 절대 복사하지 않는다.
/// 서버 정의 안의 env·headers는 사용자가 공유하는 MCP 설정의 일부다.
fn merge_claude_mcp(home: &Path, profile: &Path, report: &mut Report) -> io::Result<()> {
    let source = match std::fs::read(home.join(".claude.json")) {
        Ok(bytes) => json_object(&bytes)?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    let Some(servers) = source.get("mcpServers") else {
        return Ok(());
    };
    let Some(servers) = servers.as_object() else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid source mcpServers",
        ));
    };
    if servers.is_empty() {
        return Ok(());
    }
    let dst = profile.join(".claude.json");
    // Claude도 literal <config>.lock 디렉터리로 잠근다(Orca claude-folder-trust-file.ts).
    // 기존 잠금은 stale 판단·삭제하지 않는다. 다음 실행에서 다시 시도한다.
    let lock = profile.join(".claude.json.lock");
    match std::fs::create_dir(&lock) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            report.conflicts.push(".claude.json");
            return Ok(());
        }
        Err(error) => return Err(error),
    }
    let _guard = McpLock(lock);
    let original = match dst.symlink_metadata() {
        Ok(metadata) if !metadata.is_file() || metadata.file_type().is_symlink() => {
            report.conflicts.push(".claude.json");
            return Ok(());
        }
        Ok(_) => Some(std::fs::read(&dst)?),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(error),
    };
    let mut config = match &original {
        Some(bytes) => json_object(bytes)?,
        None => serde_json::Map::new(),
    };
    let current = config
        .entry("mcpServers")
        .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
    let Some(current) = current.as_object_mut() else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid profile mcpServers",
        ));
    };
    let mut changed = false;
    let mut conflict = false;
    for (name, server) in servers {
        if let Some(own) = current.get(name) {
            conflict |= own != server;
        } else {
            current.insert(name.clone(), server.clone());
            changed = true;
        }
    }
    if conflict {
        report.conflicts.push(".claude.json");
    }
    if changed {
        let bytes = serde_json::to_vec_pretty(&config)?;
        publish_mcp(&dst, &bytes, original.as_deref())?;
        report.changed.push(".claude.json");
    }
    Ok(())
}

fn json_object(bytes: &[u8]) -> io::Result<serde_json::Map<String, serde_json::Value>> {
    let value: serde_json::Value = serde_json::from_slice(bytes)?;
    value.as_object().cloned().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "claude state is not a JSON object",
        )
    })
}

/// 잠금 취득에 성공한 호출만 해제한다. 조기 반환·오류에도 기존 잠금은 건드리지 않는다.
struct McpLock(std::path::PathBuf);

impl Drop for McpLock {
    fn drop(&mut self) {
        std::fs::remove_dir(&self.0).ok();
    }
}

/// 신규 파일은 `hard_link`로 무덮어쓰기 게시, 기존 파일은 Claude 잠금 아래 rename한다.
/// 잠금을 지키지 않는 외부 쓰기도 발견하면 거부한다. 원본/심볼릭 링크 대상에는 쓰지 않는다.
fn publish_mcp(dst: &Path, bytes: &[u8], original: Option<&[u8]>) -> io::Result<()> {
    use krill_proto::os::fs::OpenOptionsExt as _;
    use std::io::Write as _;
    use std::sync::atomic::{AtomicU64, Ordering};

    static SEQ: AtomicU64 = AtomicU64::new(0);
    let tmp = dst.with_file_name(format!(
        ".claude.json.krill-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&tmp)?;
    let result = (|| {
        #[cfg(unix)]
        file.set_permissions(std::os::unix::fs::PermissionsExt::from_mode(0o600))?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        if let Some(original) = original {
            let metadata = dst.symlink_metadata()?;
            if !metadata.is_file()
                || metadata.file_type().is_symlink()
                || std::fs::read(dst)? != original
            {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "profile state changed during merge",
                ));
            }
            std::fs::rename(&tmp, dst)
        } else {
            std::fs::hard_link(&tmp, dst)
        }
    })();
    std::fs::remove_file(&tmp).ok();
    result
}

/// 없으면 링크, 빈 폴더면 링크로 바꾼다(기존 빈 프로필 이전). 같은 링크면 그대로, 그 밖은 충돌.
#[cfg(not(unix))]
fn link(src: &Path, dst: &Path, name: &'static str, report: &mut Report) -> io::Result<()> {
    match dst.symlink_metadata() {
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
        Ok(m) if m.file_type().is_symlink() => {
            if std::fs::read_link(dst)? != src {
                report.conflicts.push(name);
            }
            return Ok(());
        }
        // `remove_dir`는 빈 폴더만 지운다. 내용이 있으면 실패하고 그대로 둔다.
        Ok(m) if m.is_dir() && std::fs::remove_dir(dst).is_ok() => {}
        Ok(_) => {
            report.conflicts.push(name);
            return Ok(());
        }
    }
    crate::os::symlink(src, dst)?;
    report.changed.push(name);
    Ok(())
}

/// `dst`가 없을 때만 임시 파일을 써서 하드 링크로 게시한다. 쓰다 죽어도 반쪽 파일이 `dst`에 남지 않고,
/// 그사이 생긴 `dst`는 덮어쓰지 않는다.
fn publish(
    dst: &Path,
    write: impl FnOnce(&Path) -> io::Result<()>,
    name: &'static str,
    report: &mut Report,
) -> io::Result<()> {
    if dst.symlink_metadata().is_ok() {
        return Ok(());
    }
    let tmp = dst.with_file_name(format!(".{name}.krill-{}", std::process::id()));
    let result = write(&tmp).and_then(|()| std::fs::hard_link(&tmp, dst));
    std::fs::remove_file(&tmp).ok();
    match result {
        Ok(()) => report.changed.push(name),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e),
    }
    Ok(())
}

#[cfg(test)]
#[cfg(unix)]
#[path = "provision/claude_tests.rs"]
mod tests;
