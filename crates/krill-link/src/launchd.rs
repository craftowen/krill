//! `krill-link enable` / `krill-link disable`: `launchd` `LaunchAgent`(`dev.krill.app.link`, `KeepAlive`) 설치·제거.
//!
//! `krill-proto::daemon::holder_plist` 및 CONVENTIONS §9(A1, A2, A3) 규칙을 따른다.
//!
//! 격리 상태 디렉터리(`KRILL_STATE_DIR`·`--state-dir`)로 켜면 라벨에 상태 디렉터리 해시를 붙이고
//! plist `EnvironmentVariables`로 `KRILL_STATE_DIR`을 넘긴다. 격리 앱이 실제 사용자 항목을 덮어쓰거나 끄지 않고,
//! launchd가 띄운 `serve`가 실제 상태 디렉터리(`link-ctl.sock`)에 붙지 않는다(OWE-669).

use krill_proto::os::ffi::OsStrExt as _;
#[cfg(target_os = "macos")]
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
#[cfg(target_os = "macos")]
use std::process::Command;

use krill_proto::daemon::BUNDLE_ID;

use crate::Error;
use crate::store::encode_hex;
#[cfg(target_os = "macos")]
use crate::store::write_atomic_0600;

/// `krill-link` `launchd` `LaunchAgent` 레이블 (CONVENTIONS A3 고유 식별자). 기본 상태 디렉터리 전용.
pub const LINK_LABEL: &str = "dev.krill.app.link";

/// launchd 항목을 따로 둘 격리 상태 디렉터리. 재정의 없는 기본 상태 디렉터리면 `None`.
pub fn isolated_state_dir(state_dir: &Path) -> Option<&Path> {
    let default = krill_proto::daemon::state_dir_override()
        .is_none()
        .then(krill_proto::daemon::state_dir)
        .flatten();
    isolated_in(state_dir, default.as_deref())
}

fn isolated_in<'a>(state_dir: &'a Path, default: Option<&Path>) -> Option<&'a Path> {
    (default != Some(state_dir)).then_some(state_dir)
}

/// `LaunchAgent` 라벨. 격리 상태 디렉터리면 `<LINK_LABEL>.<경로 SHA-256 앞 8바이트 hex>`.
pub fn link_label(isolated: Option<&Path>) -> String {
    match isolated {
        None => LINK_LABEL.to_owned(),
        Some(dir) => {
            let digest = ring::digest::digest(&ring::digest::SHA256, dir.as_os_str().as_bytes());
            format!("{LINK_LABEL}.{}", encode_hex(&digest.as_ref()[..8]))
        }
    }
}

/// `launchd` 잡 소프트 fd 한도 (`krill-proto::daemon`과 동일).
const NOFILE: u32 = 10_240;

/// `krill-link serve`용 `launchd` `LaunchAgent` plist XML을 생성한다.
///
/// - `RunAtLoad = true`, `KeepAlive = true`로 원격 연결이 켜져 있는 동안 상시 유지한다.
/// - `AssociatedBundleIdentifiers = [BUNDLE_ID]`로 macOS 로그인 항목 귀속을 명시한다(CONVENTIONS A2).
/// - `launchd`가 띄우므로 `--detach`나 자체 `setsid`를 쓰지 않는다(CONVENTIONS A1).
/// - 격리 상태 디렉터리면 라벨을 바꾸고 `EnvironmentVariables`에 `KRILL_STATE_DIR`을 넣는다.
pub fn link_plist(krill_link_exe: &str, isolated: Option<&str>) -> String {
    let mut args = String::new();
    for a in [krill_link_exe, "serve"] {
        args.push_str("\t\t<string>");
        args.push_str(&escape_xml(a));
        args.push_str("</string>\n");
    }
    let label = link_label(isolated.map(Path::new));
    let env = isolated.map_or_else(String::new, |dir| {
        format!(
            "\t<key>EnvironmentVariables</key>\n\t<dict>\n\t\t<key>{}</key>\n\t\t<string>{}</string>\n\t</dict>\n",
            "KRILL_STATE_DIR",
            escape_xml(dir)
        )
    });
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>Label</key>
	<string>{label}</string>
	<key>ProgramArguments</key>
	<array>
{args}	</array>
{env}	<key>RunAtLoad</key>
	<true/>
	<key>KeepAlive</key>
	<true/>
	<key>ProcessType</key>
	<string>Interactive</string>
	<key>AssociatedBundleIdentifiers</key>
	<array>
		<string>{BUNDLE_ID}</string>
	</array>
	<key>SoftResourceLimits</key>
	<dict>
		<key>NumberOfFiles</key>
		<integer>{NOFILE}</integer>
	</dict>
</dict>
</plist>
"#
    )
}

fn escape_xml(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            c => out.push(c),
        }
    }
    out
}

/// `LaunchAgent` plist 파일 경로를 돌려준다.
///
/// 테스트 격리를 위해 `KRILL_LINK_LAUNCH_AGENTS_DIR`이 설정돼 있으면 그 디렉터리를 쓰고,
/// 없으면 `~/Library/LaunchAgents/<label>.plist`를 돌려준다.
pub fn launch_agent_plist_path(label: &str) -> Result<PathBuf, Error> {
    if let Some(dir) = std::env::var_os("KRILL_LINK_LAUNCH_AGENTS_DIR") {
        return Ok(PathBuf::from(dir).join(format!("{label}.plist")));
    }
    let home = std::env::var_os("HOME")
        .ok_or_else(|| Error::Io(io::Error::new(io::ErrorKind::NotFound, "HOME is not set")))?;
    Ok(PathBuf::from(home)
        .join("Library/LaunchAgents")
        .join(format!("{label}.plist")))
}

/// `state_dir`용 `LaunchAgent` plist를 설치하고 `launchctl bootstrap gui/<uid>`로 활성화한다.
#[cfg(target_os = "macos")]
pub fn enable_launch_agent(krill_link_exe: &Path, state_dir: &Path) -> Result<PathBuf, Error> {
    let isolated = match isolated_state_dir(state_dir) {
        None => None,
        Some(dir) => Some(dir.to_str().ok_or_else(|| {
            Error::InvalidConfig("state directory path is not valid UTF-8".to_owned())
        })?),
    };
    let label = link_label(isolated.map(Path::new));
    let plist_path = launch_agent_plist_path(&label)?;
    let exe_str = krill_link_exe.to_str().ok_or_else(|| {
        Error::InvalidConfig("krill-link executable path is not valid UTF-8".to_owned())
    })?;
    let plist_xml = link_plist(exe_str, isolated);
    write_atomic_0600(&plist_path, plist_xml.as_bytes()).map_err(Error::Io)?;

    if std::env::var_os("KRILL_LINK_SKIP_LAUNCHCTL").is_none() {
        let uid = rustix::process::getuid().as_raw();
        let domain = format!("gui/{uid}");
        let target = format!("{domain}/{label}");
        // 이미 등록돼 있으면 먼저 bootout 후 다시 bootstrap한다.
        Command::new("/bin/launchctl")
            .args(["bootout", &target])
            .output()
            .ok();
        let out = Command::new("/bin/launchctl")
            .args(["bootstrap", &domain])
            .arg(&plist_path)
            .output()
            .map_err(Error::Io)?;
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr).trim().to_owned();
            return Err(Error::Launchd(format!(
                "launchctl bootstrap {domain} failed ({}): {stderr}",
                out.status
            )));
        }
    }
    Ok(plist_path)
}

/// `state_dir`용 `launchctl bootout gui/<uid>/<label>`로 중지하고 `LaunchAgent` plist 파일을 제거한다.
#[cfg(target_os = "macos")]
pub fn disable_launch_agent(state_dir: &Path) -> Result<PathBuf, Error> {
    let label = link_label(isolated_state_dir(state_dir));
    let plist_path = launch_agent_plist_path(&label)?;
    if std::env::var_os("KRILL_LINK_SKIP_LAUNCHCTL").is_none() {
        let uid = rustix::process::getuid().as_raw();
        let target = format!("gui/{uid}/{label}");
        Command::new("/bin/launchctl")
            .args(["bootout", &target])
            .output()
            .ok();
    }
    if plist_path.exists() {
        fs::remove_file(&plist_path).map_err(Error::Io)?;
    }
    Ok(plist_path)
}

/// `LaunchAgent` installation is only supported on macOS.
#[cfg(not(target_os = "macos"))]
pub fn enable_launch_agent(_krill_link_exe: &Path, _state_dir: &Path) -> Result<PathBuf, Error> {
    Err(crate::unsupported_platform())
}

/// `LaunchAgent` removal is only supported on macOS.
#[cfg(not(target_os = "macos"))]
pub fn disable_launch_agent(_state_dir: &Path) -> Result<PathBuf, Error> {
    Err(crate::unsupported_platform())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn link_plist_matches_snapshot() {
        let generated = link_plist(
            "/Applications/Krill.app/Contents/Helpers/R&D/krill-link",
            None,
        );
        assert_eq!(generated, include_str!("launchd/link.plist"));
        assert!(generated.contains("<key>AssociatedBundleIdentifiers</key>"));
        assert!(generated.contains("<key>KeepAlive</key>\n\t<true/>"));
        assert!(!generated.contains("--detach"));
        assert!(!generated.contains("EnvironmentVariables"));
    }

    /// OWE-669: 격리 상태 디렉터리는 실제 라벨과 겹치지 않는 라벨 + `KRILL_STATE_DIR` 환경으로 띄운다.
    #[test]
    fn isolated_plist_carries_state_dir_and_distinct_label() {
        let dir = "/tmp/krill-qa/R&D state";
        let generated = link_plist("/x/krill-link", Some(dir));
        let label = link_label(Some(Path::new(dir)));
        assert_ne!(label, LINK_LABEL);
        assert!(label.starts_with(&format!("{LINK_LABEL}.")));
        assert_eq!(label.len(), LINK_LABEL.len() + 1 + 16);
        assert_eq!(label, link_label(Some(Path::new(dir))), "deterministic");
        assert_ne!(label, link_label(Some(Path::new("/tmp/other"))));
        assert!(generated.contains(&format!("<key>Label</key>\n\t<string>{label}</string>")));
        assert!(generated.contains(
            "<key>EnvironmentVariables</key>\n\t<dict>\n\t\t<key>KRILL_STATE_DIR</key>\n\t\t<string>/tmp/krill-qa/R&amp;D state</string>\n\t</dict>"
        ));
    }

    #[test]
    fn only_default_state_dir_uses_real_label() {
        let real = Path::new("/Users/u/Library/Application Support/dev.krill.app");
        let iso = Path::new("/tmp/krill-qa");
        assert_eq!(isolated_in(real, Some(real)), None);
        assert_eq!(isolated_in(iso, Some(real)), Some(iso));
        // 재정의가 있으면(기본값 없음) 어떤 상태 디렉터리도 격리로 본다.
        assert_eq!(isolated_in(real, None), Some(real));
    }

    /// 이관(MK-170)은 `<번들 id>.link`로 새 라벨을 만든다.
    #[test]
    fn link_label_follows_bundle_id() {
        assert_eq!(LINK_LABEL, format!("{BUNDLE_ID}.link"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn link_plist_passes_macos_plutil_lint() {
        let dir = std::env::temp_dir().join(format!("mklink-plist-{}", std::process::id()));
        fs::remove_dir_all(&dir).ok();
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("dev.krill.app.link.plist");
        fs::write(&file, link_plist("/a&b/krill-link", None)).unwrap();
        let out = Command::new("/usr/bin/plutil")
            .arg("-lint")
            .arg(&file)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "plutil failed: {}",
            String::from_utf8_lossy(&out.stdout)
        );
        fs::write(&file, link_plist("/a&b/krill-link", Some("/tmp/a&b"))).unwrap();
        let out = Command::new("/usr/bin/plutil")
            .arg("-lint")
            .arg(&file)
            .output()
            .unwrap();
        assert!(out.status.success(), "isolated plutil failed");
        fs::remove_dir_all(&dir).ok();
    }
}
