//! 설치된 프로바이더 감지(OWE-706).
//!
//! 감지는 온보딩과 같은 경로를 쓴다: 로그인 셸 PATH([`crate::onboarding::login_path`]) +
//! [`krill_proto::agent::which`]. 결과는 앱 전역 [`Installed`]에 두고, 감지 전(`None`)에는
//! 모든 프로바이더를 설치된 것으로 본다(기존 화면 그대로).

use std::ffi::{OsStr, OsString};

use gpui::App;
use krill_proto::usage::ProviderKind;

/// 감지 결과. `None`이면 아직 모른다.
#[derive(Debug, Clone, Default)]
pub struct Installed(pub Option<Vec<ProviderKind>>);

impl gpui::Global for Installed {}

/// `path`(PATH 형식)에서 실행 파일이 보이는 프로바이더. 실행 파일 이름은 `ProviderKind::as_str`.
#[must_use]
pub fn detect(path: &OsStr) -> Vec<ProviderKind> {
    ProviderKind::ALL
        .into_iter()
        .filter(|p| krill_proto::agent::which(p.as_str(), path).is_some())
        .collect()
}

/// 감지 전이거나 감지에서 찾았으면 `true`.
#[must_use]
pub fn is_installed(cx: &App, provider: ProviderKind) -> bool {
    cx.try_global::<Installed>()
        .and_then(|i| i.0.as_ref())
        .is_none_or(|found| found.contains(&provider))
}

/// 로그인 셸 PATH로 백그라운드 감지해 [`Installed`]에 저장한다. UI 스레드를 막지 않는다.
pub fn spawn_detect(shell: OsString, cx: &mut App) {
    cx.spawn(async move |cx| {
        let found = cx
            .background_executor()
            .spawn(async move { detect_login(crate::onboarding::login_path(&shell)) })
            .await;
        // 로그인 셸 PATH를 못 얻으면 모름으로 둔다. GUI 최소 PATH로 찾으면 전부 "설치 안 됨"으로 접힌다.
        if found.is_some() {
            cx.update(|cx| cx.set_global(Installed(found)));
        }
    })
    .detach();
}

/// 로그인 셸 PATH가 있을 때만 감지한다. 없으면 `None`(모름 = 전부 설치로 본다).
fn detect_login(login: Option<OsString>) -> Option<Vec<ProviderKind>> {
    login.map(|path| detect(&path))
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt as _;

    use super::*;

    #[test]
    fn detect_finds_only_executables_on_path() {
        let d = std::env::temp_dir().join(format!("mk-installed-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        for (name, mode) in [("opencode", 0o755), ("claude", 0o755), ("codex", 0o644)] {
            let f = d.join(name);
            std::fs::write(&f, "#!/bin/sh\n").unwrap();
            std::fs::set_permissions(&f, std::fs::Permissions::from_mode(mode)).unwrap();
        }
        assert_eq!(
            detect(d.as_os_str()),
            vec![ProviderKind::Claude, ProviderKind::OpenCode]
        );
        assert!(detect(OsStr::new("")).is_empty());
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn missing_login_path_stays_unknown() {
        assert_eq!(detect_login(None), None);
        assert_eq!(detect_login(Some(OsString::new())), Some(Vec::new()));
    }

    #[gpui::test]
    fn unknown_detection_counts_as_installed(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            assert!(is_installed(cx, ProviderKind::Codex));
            cx.set_global(Installed(Some(vec![ProviderKind::Claude])));
            assert!(is_installed(cx, ProviderKind::Claude));
            assert!(!is_installed(cx, ProviderKind::Codex));
        });
    }
}
