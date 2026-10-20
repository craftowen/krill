//! Native npm launcher regression: no `/usr/bin/env`, POSIX shell, or extensionless executable.

use super::*;

#[test]
fn windows_login_and_probe_share_the_selected_profile_and_native_home() {
    for provider in [ProviderKind::Claude, ProviderKind::Codex] {
        let root = std::env::temp_dir().join(format!(
            "krill login 한글 {} {}",
            provider.as_str(),
            std::process::id()
        ));
        let profile = root.join("profile with spaces & data");
        let host = root.join(format!(".{}", provider.as_str()));
        std::fs::create_dir_all(&profile).unwrap();
        std::fs::create_dir_all(&host).unwrap();
        let exe = root.join(format!("{}.cmd", provider.as_str()));
        let output = if provider == ProviderKind::Claude {
            "echo {\"loggedIn\":true,\"email\":\"fixture@example.test\"}"
        } else {
            "echo Logged in using ChatGPT 1>&2"
        };
        let script = format!(
            "@echo off\r\nsetlocal\r\nset \"KRILL_TEST_DIR=%{config}%\"\r\nif not defined KRILL_TEST_DIR set \"KRILL_TEST_DIR=%USERPROFILE%\\.{name}\"\r\nif \"%~2\"==\"status\" goto status\r\n> \"%KRILL_TEST_DIR%\\authenticated\" echo ready\r\nexit /b 0\r\n:status\r\nif not exist \"%KRILL_TEST_DIR%\\authenticated\" exit /b 9\r\n{output}\r\nexit /b 0\r\n",
            config = provider.env_var(),
            name = provider.as_str(),
        );
        std::fs::write(&exe, script).unwrap();
        let resolved = crate::agent::which(provider.as_str(), root.as_os_str()).unwrap();
        assert_eq!(resolved, exe);
        for dir in [Some(profile.as_path()), None] {
            let mut login = login_command(&resolved, provider, dir).unwrap();
            crate::os::configure_background(&mut login);
            assert!(login.env("USERPROFILE", &root).status().unwrap().success());
            let auth = probe_with_environment(&resolved, provider, dir, Some(&root), None).unwrap();
            assert!(auth.logged_in);
        }
        assert!(profile.join("authenticated").is_file());
        assert!(host.join("authenticated").is_file());
        std::fs::remove_dir_all(root).unwrap();
    }
}
