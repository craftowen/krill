//! 설치 출처와 플랫폼에 맞는 명령 선택. 파일 조회는 백그라운드 스캔에서만 한다.
use std::ffi::OsStr;
use std::path::Path;
#[cfg(windows)]
use std::path::PathBuf;

use super::ToolSpec;
use crate::i18n::catalog::settings as text;

pub(super) fn command(spec: &ToolSpec, path: &OsStr) -> String {
    if cfg!(windows) {
        return spec.install_windows.to_owned();
    }
    if spec.id == "pkgmgr" && krill_proto::agent::which("brew", path).is_some() {
        return "brew update".to_owned();
    }
    // PATH 앞의 사용자 래퍼를 지나 실제 패키지의 심볼릭 링크까지 확인한다.
    for bin in spec.probe_bins() {
        for dir in std::env::split_paths(path) {
            let Ok(target) = dir.join(bin).canonicalize() else {
                continue;
            };
            if let Some(cmd) = brew_update(&target) {
                return cmd;
            }
            // npm 설치가 앞서면 뒤에 남은 Homebrew 설치를 업데이트하지 않는다.
            if target
                .components()
                .any(|part| part.as_os_str() == "node_modules")
            {
                return spec.install_macos.to_owned();
            }
        }
    }
    if spec.id == "codex" && krill_proto::agent::which("brew", path).is_some() {
        return "brew update && brew install --cask codex".to_owned();
    }
    spec.install_macos.to_owned()
}

fn brew_update(binary: &Path) -> Option<String> {
    for parent in binary.ancestors() {
        let folder = parent.parent()?;
        let kind = folder.file_name()?.to_str()?;
        if !matches!(kind, "Cellar" | "Caskroom") {
            continue;
        }
        let name = parent.file_name()?.to_str()?;
        let brew = folder.parent()?.join("bin/brew");
        if !brew.is_file() {
            return None;
        }
        let brew = krill_proto::agent::hooks::shell_quote(brew.to_str()?);
        if kind == "Caskroom" && name == "claude-code" {
            // 먼저 latest를 내려받는다. 전환 실패 시 기존 설치를 복구하되 오류 코드를 유지한다.
            return Some(format!(
                "{brew} update && {brew} fetch --cask 'claude-code@latest' && {brew} uninstall --cask 'claude-code' && ( {brew} install --cask 'claude-code@latest' || {{ krill_latest_status=$?; {brew} install --cask 'claude-code'; exit \"$krill_latest_status\"; }} )"
            ));
        }
        let name = krill_proto::agent::hooks::shell_quote(name);
        let kind = if kind == "Caskroom" {
            "--cask"
        } else {
            "--formula"
        };
        let refresh =
            if kind == "--cask" && matches!(name.as_str(), "'codex'" | "'claude-code@latest'") {
                format!("{brew} update && ")
            } else {
                String::new()
            };
        return Some(format!("{refresh}{brew} upgrade {kind} {name}"));
    }
    None
}

pub(super) fn prerequisite(spec: &ToolSpec, path: &OsStr) -> Option<String> {
    let cmd = command(spec, path);
    let required = if cfg!(windows) {
        if spec.id == "pkgmgr" {
            return Some(text::INSTALL_WINGET_MANUAL.get().to_owned());
        }
        if cmd.starts_with("winget ") {
            Some("winget")
        } else if cmd.starts_with("npm.cmd ") {
            Some("npm")
        } else {
            None
        }
    } else if spec.id == "pkgmgr" && krill_proto::agent::which("brew", path).is_none() {
        return Some(text::INSTALL_BREW_TERMINAL.get().to_owned());
    } else if cmd.starts_with("brew ") {
        Some("brew")
    } else if cmd.starts_with("npm ") {
        Some("npm")
    } else {
        None
    };
    required
        .filter(|bin| krill_proto::agent::which(bin, path).is_none())
        .map(|bin| {
            text::INSTALL_MISSING_PREREQUISITE
                .get()
                .replace("{tool}", bin)
        })
}

/// 설치 직후 Explorer의 오래된 PATH 대신 레지스트리에 기록된 경로를 합친다.
#[cfg(windows)]
pub(super) fn windows_registry_paths() -> Vec<PathBuf> {
    let mut cmd = super::runner::powershell_command(
        "foreach ($target in 'Machine','User') { $value=[Environment]::GetEnvironmentVariable('Path',$target); if ($value) { [Environment]::ExpandEnvironmentVariables($value) } }",
    );
    let Ok(out) = super::super::child::output(&mut cmd, std::time::Duration::from_secs(4), 32768)
    else {
        return Vec::new();
    };
    if !out.status.success() {
        return Vec::new();
    }
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .flat_map(|line| std::env::split_paths(OsStr::new(line.trim())).collect::<Vec<_>>())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn brew_updates_the_owning_package_and_quotes_paths() {
        let root = std::env::temp_dir().join(format!("krill-install-'{}", std::process::id()));
        std::fs::create_dir_all(root.join("bin")).unwrap();
        std::fs::write(root.join("bin/brew"), "").unwrap();
        let quote = krill_proto::agent::hooks::shell_quote;
        assert_eq!(
            brew_update(&root.join("Caskroom/codex/1/bin/codex")),
            Some(format!(
                "{0} update && {0} upgrade --cask 'codex'",
                quote(root.join("bin/brew").to_str().unwrap())
            ))
        );
        assert_eq!(
            brew_update(&root.join("Cellar/node/1/bin/node")),
            Some(format!(
                "{} upgrade --formula 'node'",
                quote(root.join("bin/brew").to_str().unwrap())
            ))
        );
        assert_eq!(
            brew_update(&root.join("lib/node_modules/@openai/codex/bin/codex.js")),
            None
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[cfg(all(test, unix))]
#[expect(
    clippy::unwrap_used,
    reason = "테스트 픽스처 생성 실패는 즉시 중단한다"
)]
mod regression_tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt as _, symlink};
    use std::sync::atomic::{AtomicUsize, Ordering};

    static SEQ: AtomicUsize = AtomicUsize::new(0);
    struct Fixture(PathBuf);
    use std::path::PathBuf;
    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "krill-install-source-{}-{}",
                std::process::id(),
                SEQ.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&root).unwrap();
            Self(root)
        }
        fn script(&self, relative: &str, content: &str) -> PathBuf {
            let path = self.0.join(relative);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, content).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            path
        }
        fn link(&self, target: &Path, relative: &str) {
            let path = self.0.join(relative);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            symlink(target, path).unwrap();
        }
        fn spec() -> &'static ToolSpec {
            super::super::CATALOG
                .iter()
                .find(|s| s.id == "codex")
                .unwrap()
        }
        fn search(&self, entries: &[&str]) -> std::ffi::OsString {
            std::env::join_paths(
                entries
                    .iter()
                    .map(|s| self.0.join(s))
                    .chain(["/bin".into(), "/usr/bin".into()]),
            )
            .unwrap()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).ok();
        }
    }

    #[test]
    fn homebrew_codex_behind_user_wrapper_never_invokes_npm() {
        let f = Fixture::new();
        f.script("user/codex", "#!/bin/sh\nprintf wrapper-version");
        let binary = f.script(
            "brew/Caskroom/codex/1/bin/codex",
            "#!/bin/sh\nprintf codex-version",
        );
        f.link(&binary, "brew/bin/codex");
        f.script(
            "brew/bin/brew",
            "#!/bin/sh\nprintf '%s\\n' \"$@\" >> invoked",
        );
        f.script("brew/bin/npm", "#!/bin/sh\nprintf collision >&2; exit 99");
        let path = f.search(&["user", "brew/bin"]);
        let status = super::super::probe_tool(Fixture::spec(), &path);
        assert_eq!(status.version.as_deref(), Some("wrapper-version"));
        let cmd = status.command.unwrap();
        let result = super::super::runner::run(&cmd, &f.0, &path);
        assert!(result.success, "{}", result.output);
        assert_eq!(
            std::fs::read_to_string(f.0.join("invoked")).unwrap(),
            "update\nupgrade\n--cask\ncodex\n"
        );
    }

    #[test]
    fn npm_install_precedes_leftover_homebrew_install() {
        let f = Fixture::new();
        let npm = f.script(
            "npm/lib/node_modules/@openai/codex/bin/codex.js",
            "#!/bin/sh\nprintf npm-version",
        );
        f.link(&npm, "npm/bin/codex");
        let brew = f.script(
            "brew/Caskroom/codex/1/bin/codex",
            "#!/bin/sh\nprintf brew-version",
        );
        f.link(&brew, "brew/bin/codex");
        f.script("brew/bin/brew", "#!/bin/sh\nexit 99");
        assert_eq!(
            command(Fixture::spec(), &f.search(&["npm/bin", "brew/bin"])),
            "npm install -g @openai/codex@latest"
        );
    }

    #[test]
    fn installed_homebrew_updates_without_bootstrap_script() {
        let f = Fixture::new();
        f.script("brew/bin/brew", "#!/bin/sh\nexit 0");
        let spec = super::super::CATALOG
            .iter()
            .find(|s| s.id == "pkgmgr")
            .unwrap();
        assert_eq!(command(spec, &f.search(&["brew/bin"])), "brew update");
    }
}

#[cfg(all(test, unix))]
#[expect(
    clippy::unwrap_used,
    reason = "테스트 픽스처 생성 실패는 즉시 중단한다"
)]
mod latest_tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt as _, symlink};

    fn migration_fixture(name: &str) -> (std::path::PathBuf, String) {
        let root = std::env::temp_dir().join(format!("krill-latest-{}-{name}", std::process::id()));
        std::fs::remove_dir_all(&root).ok();
        std::fs::create_dir_all(root.join("bin")).unwrap();
        let source = root.join("Caskroom/claude-code/1/bin/claude");
        std::fs::create_dir_all(source.parent().unwrap()).unwrap();
        std::fs::write(&source, "#!/bin/sh\nexit 0\n").unwrap();
        symlink(&source, root.join("bin/claude")).unwrap();
        let brew = root.join("bin/brew");
        std::fs::write(&brew, r#"#!/bin/sh
printf '%s\n' "$*" >> invoked
case "$*" in
  'fetch --cask claude-code@latest') if [ -f fail_fetch ]; then exit 8; fi ;;
  'uninstall --cask claude-code') rm installed ;;
  'install --cask claude-code@latest') if [ -f fail_latest ]; then exit 9; fi; printf latest > installed ;;
  'install --cask claude-code') printf stable > installed ;;
esac
"#).unwrap();
        std::fs::set_permissions(brew, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::write(root.join("installed"), "stable").unwrap();
        let cmd = brew_update(&source).unwrap();
        (root, cmd)
    }

    fn execute(root: &Path, cmd: &str) -> super::super::InstallJobResult {
        super::super::runner::run(cmd, root, OsStr::new("/bin:/usr/bin"))
    }

    #[test]
    fn claude_stable_cask_migrates_to_latest() {
        let (root, cmd) = migration_fixture("success");
        let result = execute(&root, &cmd);
        assert!(result.success, "{}", result.output);
        assert_eq!(
            std::fs::read_to_string(root.join("installed")).unwrap(),
            "latest"
        );
        assert_eq!(
            std::fs::read_to_string(root.join("invoked")).unwrap(),
            "update\nfetch --cask claude-code@latest\nuninstall --cask claude-code\ninstall --cask claude-code@latest\n"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn claude_migration_download_failure_keeps_existing_installation() {
        let (root, cmd) = migration_fixture("download-failure");
        std::fs::write(root.join("fail_fetch"), "").unwrap();
        assert!(!execute(&root, &cmd).success);
        assert_eq!(
            std::fs::read_to_string(root.join("installed")).unwrap(),
            "stable"
        );
        assert!(
            !std::fs::read_to_string(root.join("invoked"))
                .unwrap()
                .contains("uninstall")
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn claude_migration_failure_restores_previous_installation_and_returns_failure() {
        let (root, cmd) = migration_fixture("install-failure");
        std::fs::write(root.join("fail_latest"), "").unwrap();
        assert!(!execute(&root, &cmd).success);
        assert_eq!(
            std::fs::read_to_string(root.join("installed")).unwrap(),
            "stable"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn existing_agy_runs_update_instead_of_noop_installer() {
        let root = std::env::temp_dir().join(format!("krill-agy-latest-{}", std::process::id()));
        std::fs::create_dir_all(root.join("bin")).unwrap();
        let binary = root.join("bin/agy");
        std::fs::write(&binary, "#!/bin/sh\nprintf '%s' \"$1\" > invoked\n").unwrap();
        std::fs::set_permissions(binary, std::fs::Permissions::from_mode(0o755)).unwrap();
        let path =
            std::env::join_paths([root.join("bin"), "/bin".into(), "/usr/bin".into()]).unwrap();
        let spec = super::super::CATALOG
            .iter()
            .find(|s| s.id == "agy")
            .unwrap();
        let result = super::super::runner::run(&command(spec, &path), &root, &path);
        assert!(result.success, "{}", result.output);
        assert_eq!(
            std::fs::read_to_string(root.join("invoked")).unwrap(),
            "update"
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
