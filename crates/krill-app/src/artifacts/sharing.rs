//! Shared CLI helpers for publishing an open file.
use crate::i18n::catalog::artifacts as text;
use crate::settings::space::CliRunner;
use crate::settings::state_dir;
use gpui::App;
use krill_proto::artifacts::{ErrorOutput, SHARING_DISABLED_CODE, ShareOutcome};
use std::ffi::OsString;
use std::path::Path;

/// CLI 실패. 공개 게이트 거부는 따로 구분해 설정 페이지로 안내한다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CliFailure {
    SharingDisabled,
    Message(String),
}

impl CliFailure {
    /// 사용자에게 보일 한 줄.
    #[must_use]
    pub fn message(&self) -> String {
        match self {
            Self::SharingDisabled => text::DISABLED.get().to_owned(),
            Self::Message(m) => text::ERROR.format(&[("error", m)]),
        }
    }
}

/// `krill artifacts <args> --json`을 실행해 성공 JSON을 `T`로, 오류 봉투를 [`CliFailure`]로 바꾼다.
fn call<T: serde::de::DeserializeOwned>(run: &CliRunner, args: &[&str]) -> Result<T, CliFailure> {
    let mut argv: Vec<OsString> = vec!["artifacts".into()];
    argv.extend(args.iter().map(OsString::from));
    argv.push("--json".into());
    let out = run
        .raw(&argv)
        .map_err(|e| CliFailure::Message(e.to_string()))?;
    if let Ok(ErrorOutput { error }) = serde_json::from_slice::<ErrorOutput>(&out) {
        return Err(if error.code == SHARING_DISABLED_CODE {
            CliFailure::SharingDisabled
        } else {
            CliFailure::Message(error.message)
        });
    }
    serde_json::from_slice(&out).map_err(|e| CliFailure::Message(e.to_string()))
}

fn path_arg(path: &Path) -> Result<&str, CliFailure> {
    path.to_str()
        .ok_or_else(|| CliFailure::Message(format!("{}: path must be UTF-8", path.display())))
}

/// 파일을 공유한다(기록이 있으면 같은 링크의 내용을 갱신한다).
///
/// # Errors
/// 게이트 거부·CLI 실패·네트워크 오류.
pub fn share_path(run: &CliRunner, path: &Path) -> Result<ShareOutcome, CliFailure> {
    call(run, &["share", path_arg(path)?])
}

/// 스테이징된 `krill` CLI 실행기. 바이너리를 못 찾으면 `None`.
#[must_use]
pub fn runner(cx: &App) -> Option<CliRunner> {
    crate::orca_import::resolve_krill_bin(cx).map(|bin| CliRunner::process(bin, state_dir(cx)))
}
