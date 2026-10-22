//! 로그인으로 확인한 이메일로 자동 이름 Claude 프로필 폴더를 옮긴다(오웬 지시 2026-10-22, Orca 패리티).
//!
//! 계정 추가 버튼은 로그인 전에 폴더를 만들어 임시 라벨(`Claude Code Profile #n`)로 이름을 짓는다.
//! 이메일이 확인되면 `<관리 루트>/claude/<이메일 슬러그>`로 옮긴다. id는 바꾸지 않는다(활성 계정·자동전환
//! 규칙이 id를 가리킨다).
//!
//! Claude Code는 macOS 키체인 서비스 이름에 `CLAUDE_CONFIG_DIR` 해시를 쓴다. 폴더만 옮기면 로그아웃되므로
//! 자격증명을 새 경로 이름으로 먼저 복사·검증한 뒤 폴더를 옮긴다. 원래 항목은 지우지 않는다.
//! 세션이 실행에 쓴 폴더(`in_use`)는 옮기지 않는다 — 실행 중인 Claude가 옛 항목의 토큰을 갱신하면 사본이 낡는다.

use std::io;
use std::path::{Path, PathBuf};

use krill_proto::usage::ProviderKind;

use super::{SAVE_LOCK, StoredAccount, load_writable, lock, profile_in_use, save_inner, slugify};

/// 프로필 폴더에 묶인 로그인 자격증명 저장소. 테스트는 가짜를 쓴다.
pub trait Vault {
    /// `from` 항목을 `to` 이름으로 복사하고 다시 읽어 검증한다. 원본이 없으면 `Ok(false)`.
    fn copy(&self, from: &str, to: &str) -> io::Result<bool>;
}

/// 실제 저장소. macOS는 로그인 키체인, 그 밖은 폴더 안 `.credentials.json`이라 폴더와 함께 옮겨진다.
#[derive(Debug)]
pub struct Keychain;

impl Vault for Keychain {
    fn copy(&self, from: &str, to: &str) -> io::Result<bool> {
        #[cfg(target_os = "macos")]
        {
            macos::copy(from, to)
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = (from, to);
            Ok(false)
        }
    }
}

/// 옮길 수 있는 계정을 모두 옮긴다. 하나라도 옮겼으면 `true`.
pub fn adopt_email_dirs(
    home: &Path,
    state_dir: Option<&Path>,
    in_use: &[PathBuf],
    vault: &dyn Vault,
) -> io::Result<bool> {
    let _guard = lock(&SAVE_LOCK);
    let mut file = load_writable(home, state_dir)?;
    let root = krill_proto::usage::profiles::managed_root(home).join(ProviderKind::Claude.as_str());
    let todo: Vec<(String, String)> = file
        .accounts
        .iter()
        .filter_map(|a| Some((a.id.clone(), candidate(home, &root, a, in_use)?)))
        .collect();
    let mut moved = false;
    for (id, email) in todo {
        let taken: Vec<PathBuf> = file
            .accounts
            .iter()
            .map(|a| PathBuf::from(&a.profile_dir))
            .collect();
        let to = free_dir(&root, &slugify(&email), &taken);
        let Some(account) = file.accounts.iter_mut().find(|a| a.id == id) else {
            continue;
        };
        let from = PathBuf::from(&account.profile_dir);
        if let Err(error) = move_profile(&from, &to, vault) {
            tracing::warn!(kind = ?error.kind(), "profile rename skipped");
            continue;
        }
        account.profile_dir = to.display().to_string();
        if let Err(error) = save_inner(home, state_dir, &file) {
            // 등록이 옛 경로를 가리키므로 폴더를 되돌린다. 키체인 사본은 무해하게 남는다.
            std::fs::rename(&to, &from).ok();
            return Err(error);
        }
        moved = true;
    }
    Ok(moved)
}

/// 이메일이 확인됐고, 관리 루트의 자동 이름 폴더이며, 세션이 쓰지 않는 Claude 계정이면 그 이메일.
fn candidate(
    home: &Path,
    root: &Path,
    account: &StoredAccount,
    in_use: &[PathBuf],
) -> Option<String> {
    if ProviderKind::parse(&account.provider) != Some(ProviderKind::Claude) {
        return None;
    }
    let email = account.email.as_deref()?.trim();
    let dir = Path::new(&account.profile_dir);
    let name = dir.file_name()?.to_str()?;
    (!email.is_empty()
        && dir.parent() == Some(root)
        && auto_name(name)
        && krill_proto::usage::profiles::is_managed(home, dir)
        && !profile_in_use(dir, in_use))
    .then(|| email.to_owned())
}

/// 계정 추가 버튼의 임시 라벨(`Claude Code Profile #n`)에서 나온 폴더 이름인가(충돌 접미사 `-2` 포함).
fn auto_name(name: &str) -> bool {
    let prefix = format!("{}-profile-", slugify(ProviderKind::Claude.display_name()));
    name.strip_prefix(&prefix).is_some_and(|rest| {
        rest.split('-')
            .all(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
    })
}

/// 디스크에도 등록에도 없는 `<root>/<slug>`(겹치면 `-2`, `-3` …).
fn free_dir(root: &Path, slug: &str, taken: &[PathBuf]) -> PathBuf {
    let mut n = 1u32;
    loop {
        let dir = if n == 1 {
            root.join(slug)
        } else {
            root.join(format!("{slug}-{n}"))
        };
        if dir.symlink_metadata().is_err() && !taken.contains(&dir) {
            return dir;
        }
        n = n.saturating_add(1);
    }
}

fn move_profile(from: &Path, to: &Path, vault: &dyn Vault) -> io::Result<()> {
    let service = |dir: &Path| {
        crate::usage::direct::claude_service(Some(dir))
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "profile path not utf-8"))
    };
    vault.copy(&service(from)?, &service(to)?)?;
    std::fs::rename(from, to)
}

#[cfg(target_os = "macos")]
mod macos {
    use std::fmt::Write as _;
    use std::io::{self, Write as _};
    use std::process::{Command, Stdio};

    use zeroize::Zeroizing;

    const SECURITY: &str = "/usr/bin/security";
    /// `errSecItemNotFound`의 `security` 종료 코드.
    const NOT_FOUND: i32 = 44;

    // Claude Code는 `/usr/bin/security`로 항목을 만들어 ACL이 그 도구만 믿는다(direct.rs와 같은 이유).
    // 비밀은 인자가 아니라 `security -i`의 표준 입력으로 넘겨 프로세스 목록에 드러나지 않게 한다.
    pub(super) fn copy(from: &str, to: &str) -> io::Result<bool> {
        let user = crate::usage::direct::claude_keychain_user();
        let Some(secret) = read(from, &user)? else {
            return Ok(false);
        };
        let mut script =
            Zeroizing::new(format!("add-generic-password -U -a {user} -s \"{to}\" -X "));
        for b in secret.iter() {
            write!(script, "{b:02x}").ok();
        }
        script.push('\n');
        let mut child = Command::new(SECURITY)
            .arg("-i")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        let written = child
            .stdin
            .take()
            .ok_or_else(|| io::Error::other("security stdin unavailable"))
            .and_then(|mut stdin| stdin.write_all(script.as_bytes()));
        if let Err(error) = written {
            child.kill().ok();
            child.wait().ok();
            return Err(error);
        }
        if !child.wait()?.success() {
            return Err(io::Error::other("keychain write failed"));
        }
        match read(to, &user)? {
            Some(copied) if *copied == *secret => Ok(true),
            _ => Err(io::Error::other("keychain copy mismatch")),
        }
    }

    fn read(service: &str, user: &str) -> io::Result<Option<Zeroizing<Vec<u8>>>> {
        let output = Command::new(SECURITY)
            .args(["find-generic-password", "-s", service, "-a", user, "-w"])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()?;
        let mut secret = Zeroizing::new(output.stdout);
        match output.status.code() {
            Some(0) => {
                if secret.last() == Some(&b'\n') {
                    secret.pop();
                }
                Ok(Some(secret))
            }
            Some(NOT_FOUND) => Ok(None),
            _ => Err(io::Error::other("keychain read failed")),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;

    /// 복사 요청을 기록하는 가짜 저장소. `fail`이면 복사를 거부한다.
    #[derive(Default)]
    struct Fake {
        calls: RefCell<Vec<(String, String)>>,
        fail: bool,
    }

    impl Vault for Fake {
        fn copy(&self, from: &str, to: &str) -> io::Result<bool> {
            self.calls
                .borrow_mut()
                .push((from.to_owned(), to.to_owned()));
            if self.fail {
                Err(io::Error::other("denied"))
            } else {
                Ok(true)
            }
        }
    }

    fn sandbox(tag: &str) -> (PathBuf, PathBuf) {
        let home =
            std::env::temp_dir().join(format!("mk-profile-name-{tag}-{}", std::process::id()));
        std::fs::remove_dir_all(&home).ok();
        let state = home.join("state");
        std::fs::create_dir_all(&state).unwrap();
        (home, state)
    }

    fn register(home: &Path, state: &Path, dir_name: &str, email: Option<&str>) -> PathBuf {
        let dir = krill_proto::usage::profiles::managed_root(home)
            .join("claude")
            .join(dir_name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("settings.json"), b"{}").unwrap();
        let mut file = load_writable(home, Some(state)).unwrap();
        file.accounts.push(StoredAccount {
            id: format!("claude-{dir_name}"),
            provider: "claude".into(),
            label: "label".into(),
            email: email.map(str::to_owned),
            profile_dir: dir.display().to_string(),
        });
        save_inner(home, Some(state), &file).unwrap();
        dir
    }

    fn stored_dir(home: &Path, state: &Path, id: &str) -> PathBuf {
        let file = load_writable(home, Some(state)).unwrap();
        PathBuf::from(
            &file
                .accounts
                .iter()
                .find(|a| a.id == id)
                .unwrap()
                .profile_dir,
        )
    }

    /// 실제 로그인 키체인에 더미 항목을 만들어 복사·검증하고 지운다. `--run-ignored only`로 수동 실행.
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "touches the real login keychain"]
    fn keychain_copy_round_trips_dummy_item() {
        use std::process::Command;
        let user = crate::usage::direct::claude_keychain_user();
        let (from, to) = (
            format!("krill-test-from-{}", std::process::id()),
            format!("krill-test-to-{}", std::process::id()),
        );
        let delete = |svc: &str| {
            Command::new("/usr/bin/security")
                .args(["delete-generic-password", "-s", svc, "-a", &user])
                .output()
                .ok();
        };
        assert!(!Keychain.copy(&from, &to).unwrap(), "원본 없음");
        let status = Command::new("/usr/bin/security")
            .args([
                "add-generic-password",
                "-U",
                "-s",
                &from,
                "-a",
                &user,
                "-w",
                r#"{"t":"a b"}"#,
            ])
            .status()
            .unwrap();
        assert!(status.success());
        let copied = Keychain.copy(&from, &to);
        let read = Command::new("/usr/bin/security")
            .args(["find-generic-password", "-s", &to, "-a", &user, "-w"])
            .output()
            .unwrap();
        delete(&from);
        delete(&to);
        assert!(copied.unwrap());
        assert_eq!(read.stdout, b"{\"t\":\"a b\"}\n");
    }

    #[test]
    fn auto_names_only() {
        assert!(auto_name("claude-code-profile-1"));
        assert!(auto_name("claude-code-profile-12-2"));
        assert!(!auto_name("claude-code-profile-"));
        assert!(!auto_name("claude-code-profile-1-work"));
        assert!(!auto_name("cheapai-api"));
        assert!(!auto_name("krill-test-example-invalid"));
    }

    #[test]
    fn moves_logged_in_auto_dir_to_email_name_with_credentials() {
        let (home, state) = sandbox("move");
        let old = register(
            &home,
            &state,
            "claude-code-profile-1",
            Some("krill-test@example.invalid"),
        );
        let vault = Fake::default();
        assert!(adopt_email_dirs(&home, Some(&state), &[], &vault).unwrap());
        let new = old.with_file_name("krill-test-example-invalid");
        assert_eq!(
            stored_dir(&home, &state, "claude-claude-code-profile-1"),
            new
        );
        assert!(!old.exists());
        assert!(new.join("settings.json").is_file());
        let service = |d: &Path| crate::usage::direct::claude_service(Some(d)).unwrap();
        assert_eq!(*vault.calls.borrow(), [(service(&old), service(&new))]);
        // 멱등: 이미 옮긴 계정은 다시 건드리지 않는다.
        assert!(!adopt_email_dirs(&home, Some(&state), &[], &vault).unwrap());
        std::fs::remove_dir_all(home).ok();
    }

    #[test]
    fn skips_in_use_unconfirmed_custom_and_failed_copies() {
        let (home, state) = sandbox("skip");
        let busy = register(&home, &state, "claude-code-profile-1", Some("a@x.com"));
        let pending = register(&home, &state, "claude-code-profile-2", None);
        let custom = register(&home, &state, "work", Some("b@x.com"));
        let vault = Fake::default();
        assert!(
            !adopt_email_dirs(&home, Some(&state), std::slice::from_ref(&busy), &vault).unwrap()
        );
        assert!(vault.calls.borrow().is_empty());
        assert!(busy.is_dir() && pending.is_dir() && custom.is_dir());

        let denied = Fake {
            fail: true,
            ..Fake::default()
        };
        assert!(!adopt_email_dirs(&home, Some(&state), &[], &denied).unwrap());
        assert_eq!(
            stored_dir(&home, &state, "claude-claude-code-profile-1"),
            busy
        );
        assert!(
            busy.is_dir(),
            "자격증명 복사가 실패하면 폴더를 옮기지 않는다"
        );
        std::fs::remove_dir_all(home).ok();
    }

    #[test]
    fn avoids_existing_folder_names() {
        let (home, state) = sandbox("collide");
        let old = register(&home, &state, "claude-code-profile-1", Some("a@x.com"));
        let existing = old.with_file_name("a-x-com");
        std::fs::create_dir_all(&existing).unwrap();
        assert!(adopt_email_dirs(&home, Some(&state), &[], &Fake::default()).unwrap());
        assert_eq!(
            stored_dir(&home, &state, "claude-claude-code-profile-1"),
            old.with_file_name("a-x-com-2")
        );
        assert!(existing.is_dir());
        std::fs::remove_dir_all(home).ok();
    }
}
