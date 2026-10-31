//! 온보딩 완료 기록(MK-161): `onboarding.toml`.
//!
//! 구 표시 파일(`onboarding.done`)은 마이그레이션 입력으로만 읽는다. `done`이
//! 있고 `toml`이 없으면 완료로 간주한다. `toml` 쓰기는 임시 파일+rename 원자 교체다.
//! 단계(`step`)는 구 4단계 마법사 파일과 호환하려고 남긴 값이다. 첫 실행 동의 단계는
//! 없어졌으므로(OWE-904) 구 `step = "consent"` 파일은 처음 상태로 읽는다.
//! 오류 보고 동의는 별도 opt-in이다. 첫 설치 후 온보딩 배너에서 한 번 묻고
//! `error_reporting_asked`에 기록한다. 기본값 OFF는 바꾸지 않는다.

use std::io;
use std::path::{Path, PathBuf};

/// 상태 파일 이름(상태 디렉터리 바로 아래).
pub const STATE_FILE: &str = "onboarding.toml";
/// 상태 문서 버전. 올리면 모르는 버전은 처음부터 다시 시작한다.
pub const STATE_VERSION: u32 = 1;

/// 저장된 단계. `toml`에는 `step = "done"` 형태로 저장된다.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Checklist,
    Connect,
    Done,
}

impl Step {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Checklist => "checklist",
            Self::Connect => "connect",
            Self::Done => "done",
        }
    }

    fn from_str(text: &str) -> Option<Self> {
        match text {
            "checklist" => Some(Self::Checklist),
            "connect" => Some(Self::Connect),
            "done" => Some(Self::Done),
            _ => None,
        }
    }
}

/// 저장되는 상태.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OnboardingState {
    pub completed: bool,
    pub step: Step,
    pub version: u32,
    /// 오류 보고 동의를 물었는가. 구 파일에는 없어 `false`(아직 묻지 않음)로 읽는다.
    pub error_reporting_asked: bool,
}

impl OnboardingState {
    fn fresh() -> Self {
        Self {
            completed: false,
            step: Step::Checklist,
            version: STATE_VERSION,
            error_reporting_asked: false,
        }
    }

    /// 완료 표시. `step`은 `Done`으로 둔다.
    #[must_use]
    pub const fn completed(mut self) -> Self {
        self.completed = true;
        self.step = Step::Done;
        self
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
struct FileState {
    completed: bool,
    step: String,
    version: u32,
    #[serde(default)]
    error_reporting_asked: bool,
}

/// 상태 디렉터리의 상태 파일 경로.
#[must_use]
pub fn state_path(state_dir: &Path) -> PathBuf {
    state_dir.join(STATE_FILE)
}

/// 상태를 읽는다. 파일 없음·파싱 실패·모르는 버전은 처음부터 시작하고,
/// 구 `done` 파일만 있으면 완료로 간주한다(마이그레이션).
#[must_use]
pub fn load(state_dir: &Path) -> OnboardingState {
    let Ok(body) = std::fs::read_to_string(state_path(state_dir)) else {
        return migrate_or_fresh(state_dir);
    };
    let Ok(file) = toml::from_str::<FileState>(&body) else {
        return OnboardingState::fresh();
    };
    if file.version != STATE_VERSION {
        return OnboardingState::fresh();
    }
    let Some(step) = Step::from_str(&file.step) else {
        return OnboardingState::fresh();
    };
    OnboardingState {
        completed: file.completed,
        step,
        version: file.version,
        error_reporting_asked: file.error_reporting_asked,
    }
}

fn migrate_or_fresh(state_dir: &Path) -> OnboardingState {
    if state_dir.join(super::DONE_FILE).exists() {
        OnboardingState {
            completed: true,
            step: Step::Done,
            version: STATE_VERSION,
            error_reporting_asked: false,
        }
    } else {
        OnboardingState::fresh()
    }
}

/// 상태를 원자 쓰기로 저장한다.
pub fn save(state_dir: &Path, state: &OnboardingState) -> io::Result<()> {
    let file = FileState {
        completed: state.completed,
        step: state.step.as_str().to_owned(),
        version: state.version,
        error_reporting_asked: state.error_reporting_asked,
    };
    let body = toml::to_string(&file).map_err(io::Error::other)?;
    atomic_write_0600(&state_path(state_dir), &body)
}

/// `settings.rs`의 `write_settings`와 같은 원자 교체다. 그 헬퍼는 settings 전용이라
/// 여기서 필요한 최소 형태(0600·고유 임시 파일·fsync·rename·실패 시 임시 파일 정리)만 둔다.
pub(crate) fn atomic_write_0600(path: &Path, body: &str) -> io::Result<()> {
    use krill_proto::os::fs::OpenOptionsExt as _;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt as _;
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);

    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension(format!(
        "tmp-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&tmp)?;
    #[cfg(unix)]
    file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    let result = (|| {
        use std::io::Write as _;
        file.write_all(body.as_bytes())?;
        file.sync_all()?;
        std::fs::rename(&tmp, path)
    })();
    if result.is_err() {
        std::fs::remove_file(&tmp).ok();
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("mk131-state-{}-{name}", std::process::id()));
        std::fs::remove_dir_all(&path).ok();
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn fresh_dir_starts_incomplete() {
        let root = dir("fresh");
        let state = load(&root);
        assert!(!state.completed);
        assert_eq!(state.step, Step::Checklist);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn legacy_done_file_migrates_to_completed() {
        let root = dir("migrate");
        std::fs::write(root.join(super::super::DONE_FILE), b"").unwrap();
        let state = load(&root);
        assert!(state.completed);
        assert_eq!(state.step, Step::Done);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn corrupt_or_unknown_state_starts_over() {
        for (name, body) in [
            ("garbage", "깨진 내용 [[["),
            (
                "unknown-step",
                "completed = false\nstep = \"warp\"\nversion = 1\n",
            ),
            (
                "removed-consent-step",
                "completed = false\nstep = \"consent\"\nversion = 1\n",
            ),
            (
                "future-version",
                "completed = true\nstep = \"done\"\nversion = 999\n",
            ),
        ] {
            let root = dir(name);
            std::fs::write(state_path(&root), body).unwrap();
            let state = load(&root);
            assert!(!state.completed, "{name}");
            assert_eq!(state.step, Step::Checklist, "{name}");
            std::fs::remove_dir_all(&root).ok();
        }
    }

    #[test]
    fn save_load_roundtrip_marks_completed() {
        let root = dir("roundtrip");
        let state = OnboardingState {
            completed: false,
            step: Step::Connect,
            version: STATE_VERSION,
            error_reporting_asked: false,
        };
        save(&root, &state).unwrap();
        assert_eq!(load(&root), state);
        let done = state.completed();
        save(&root, &done).unwrap();
        let back = load(&root);
        assert!(back.completed);
        assert_eq!(back.step, Step::Done);
        std::fs::remove_dir_all(&root).ok();
    }
}
