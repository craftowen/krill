//! MK-034: 로그인 셸 argv, 환경 보정, 오염된 부모 환경에서도 rc 파일이 같은 PATH를 만드는지.

use std::os::unix::ffi::OsStrExt;
use std::process::Command;

use super::*;

fn kv(k: &str, v: &str) -> (Vec<u8>, Vec<u8>) {
    (k.as_bytes().to_vec(), v.as_bytes().to_vec())
}

fn get<'a>(l: &'a Launch, key: &str) -> Option<&'a [u8]> {
    l.env
        .iter()
        .find(|(k, _)| k == key.as_bytes())
        .map(|(_, v)| v.as_slice())
}

fn owen() -> User {
    User {
        name: b"owen".to_vec(),
        home: b"/Users/owen".to_vec(),
        shell: b"/opt/homebrew/bin/fish".to_vec(),
    }
}

/// 개발 셸(nvm·mise가 활성화된 터미널)에서 krilld를 띄웠을 때의 환경.
fn polluted() -> Vec<(Vec<u8>, Vec<u8>)> {
    vec![
        kv("PATH", "/polluted/nvm/bin:/polluted/mise/bin:/usr/bin:/bin"),
        kv("NVM_BIN", "/polluted/nvm/bin"),
        kv("NVM_DIR", "/polluted/nvm"),
        kv("__MISE_DIFF", "xyz"),
        kv("__MISE_ORIG_PATH", "/usr/bin"),
        kv("MISE_SHELL", "zsh"),
        kv("KRILL_SESSION", "internal"),
        kv("ZDOTDIR", "/polluted/zdot"),
        kv("XPC_SERVICE_NAME", "application.dev"),
        kv("HOME", "/Users/other"),
        kv("SHELL", "/bin/bash"),
        kv("TMPDIR", "/var/folders/x/T/"),
        kv("SSH_AUTH_SOCK", "/tmp/agent"),
        kv("TERM", "dumb"),
    ]
}

#[test]
fn login_shell_from_passwd_with_dash_l() {
    let l = Shell::Login.launch_with(Some(&owen()), polluted());
    assert_eq!(l.argv, [&b"/opt/homebrew/bin/fish"[..], b"-l"]);
    assert_eq!(get(&l, "SHELL"), Some(&b"/opt/homebrew/bin/fish"[..]));
}

#[test]
fn custom_shell_passes_args_and_keeps_login_shell_var() {
    let shell = Shell::Custom {
        program: b"/bin/bash".to_vec(),
        args: vec![b"--norc".to_vec()],
    };
    let l = shell.launch_with(Some(&owen()), polluted());
    assert_eq!(l.argv, [&b"/bin/bash"[..], b"--norc"]);
    // Terminal.app도 사용자 지정 셸에서 SHELL은 로그인 셸이다.
    assert_eq!(get(&l, "SHELL"), Some(&b"/opt/homebrew/bin/fish"[..]));
}

#[test]
fn env_is_launchd_baseline_only() {
    let l = Shell::Login.launch_with_lang(Some(&owen()), polluted(), None);
    for gone in [
        "NVM_BIN",
        "NVM_DIR",
        "__MISE_DIFF",
        "__MISE_ORIG_PATH",
        "MISE_SHELL",
        "KRILL_SESSION",
        "ZDOTDIR",
        "XPC_SERVICE_NAME",
    ] {
        assert_eq!(get(&l, gone), None, "{gone}");
    }
    assert_eq!(get(&l, "PATH"), Some(LAUNCHD_PATH));
    assert_eq!(get(&l, "HOME"), Some(&b"/Users/owen"[..]));
    assert_eq!(get(&l, "USER"), Some(&b"owen"[..]));
    assert_eq!(get(&l, "LOGNAME"), Some(&b"owen"[..]));
    assert_eq!(get(&l, "TMPDIR"), Some(&b"/var/folders/x/T/"[..]));
    assert_eq!(get(&l, "SSH_AUTH_SOCK"), Some(&b"/tmp/agent"[..]));
    assert_eq!(get(&l, "TERM"), Some(&b"xterm-256color"[..]));
    assert_eq!(get(&l, "LANG"), Some(FALLBACK_LANG));
    // 키는 한 번씩만.
    let mut keys: Vec<_> = l.env.iter().map(|(k, _)| k).collect();
    let n = keys.len();
    keys.sort();
    keys.dedup();
    assert_eq!(keys.len(), n);
}

#[test]
fn inherited_locale_wins_over_fallback() {
    let mut env = polluted();
    env.push(kv("LC_CTYPE", "ko_KR.UTF-8"));
    let l = Shell::Login.launch_with_lang(Some(&owen()), env, Some(b"ko_KR.UTF-8".to_vec()));
    assert_eq!(get(&l, "LC_CTYPE"), Some(&b"ko_KR.UTF-8"[..]));
    assert_eq!(get(&l, "LANG"), None);
}

#[test]
fn system_locale_lang_used_when_nothing_inherited() {
    let l = Shell::Login.launch_with_lang(Some(&owen()), polluted(), Some(b"ko_KR.UTF-8".to_vec()));
    assert_eq!(get(&l, "LANG"), Some(&b"ko_KR.UTF-8"[..]));
}

#[test]
fn lang_from_locale_table() {
    let cases: &[(&[u8], Option<&[u8]>)] = &[
        (b"ko_KR", Some(b"ko_KR.UTF-8".as_slice())),
        (b"en_US", Some(b"en_US.UTF-8".as_slice())),
        (b"ja_JP", Some(b"ja_JP.UTF-8".as_slice())),
        (b"C", Some(b"C.UTF-8".as_slice())),
        (b"ko_KR.UTF-8", Some(b"ko_KR.UTF-8".as_slice())),
        (b"ko_KR@collation=search", Some(b"ko_KR.UTF-8".as_slice())),
        (b"", None),
        (b"ko KR", None),
        (b"ko_KR;rm -rf /", None),
    ];
    for (input, want) in cases {
        assert_eq!(
            lang_from_locale(input).as_deref(),
            *want,
            "{:?}",
            String::from_utf8_lossy(input)
        );
    }
    let long = vec![b'a'; LOCALE_MAX + 1];
    assert_eq!(lang_from_locale(&long), None);
}

#[test]
fn system_lang_falls_back_when_unreadable() {
    let l = Shell::Login.launch_with_lang(Some(&owen()), polluted(), None);
    assert_eq!(get(&l, "LANG"), Some(FALLBACK_LANG));
}

#[test]
#[cfg_attr(not(target_os = "macos"), ignore = "macOS 전용: AppleLocale(defaults)")]
fn real_system_lang_matches_this_mac() {
    // 이 맥 실측: AppleLocale=ko_KR. 다른 맥에서는 그 맥의 값과 같아야 한다.
    let got = system_lang();
    let locale = system_locale();
    match locale.as_deref().map(lang_from_locale) {
        Some(Some(want)) => assert_eq!(got, Some(want)),
        _ => assert_eq!(got, None),
    }
    if std::env::var_os("AppleLocale").is_none() {
        assert_eq!(
            got,
            Some(b"ko_KR.UTF-8".to_vec()),
            "AppleLocale env가 없을 때 defaults read 값이 이 맥 실측(ko_KR.UTF-8)과 같아야 한다"
        );
    }
}

#[test]
fn no_passwd_falls_back_to_env_then_zsh() {
    let l = Shell::Login.launch_with(None, polluted());
    assert_eq!(l.argv, [&b"/bin/bash"[..], b"-l"]);
    assert_eq!(get(&l, "HOME"), Some(&b"/Users/other"[..]));
    let l = Shell::Login.launch_with(None, Vec::new());
    assert_eq!(l.argv, [FALLBACK_SHELL, b"-l"]);
}

#[test]
fn passwd_lookup_matches_this_user() {
    let u = sys::user().unwrap();
    assert!(
        u.home.starts_with(b"/"),
        "{:?}",
        String::from_utf8_lossy(&u.home)
    );
    assert!(u.shell.starts_with(b"/"));
    assert!(!u.name.is_empty());
}

/// rc 파일이 nvm·mise처럼 PATH 앞에 붙이는 ZDOTDIR을 만든다. 사용자 rc는 읽지 않는다.
fn zdotdir(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("mk034-{}-{tag}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(
        d.join(".zshrc"),
        "export PATH=\"$ZDOTDIR/nvm/versions/node/v22/bin:$ZDOTDIR/mise/installs/node/22/bin:$PATH\"\n",
    )
    .unwrap();
    d
}

/// 실제 zsh를 로그인·대화형으로 띄워 PATH를 받는다.
fn zsh_path(argv: &[Vec<u8>], env: &[(Vec<u8>, Vec<u8>)]) -> String {
    let (prog, args) = argv.split_first().unwrap();
    let out = Command::new(std::ffi::OsStr::from_bytes(prog))
        .args(args.iter().map(|a| std::ffi::OsStr::from_bytes(a)))
        .args(["-i", "-c", "print -r -- \"MKPATH=$PATH\""])
        .env_clear()
        .envs(env.iter().map(|(k, v)| {
            (
                std::ffi::OsStr::from_bytes(k),
                std::ffi::OsStr::from_bytes(v),
            )
        }))
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.lines()
        .find_map(|l| l.strip_prefix("MKPATH="))
        .unwrap_or_else(|| panic!("PATH 없음: {text:?}"))
        .to_owned()
}

#[test]
#[cfg_attr(
    not(target_os = "macos"),
    ignore = "macOS 전용: zsh ZDOTDIR·launchd GUI PATH 비교"
)]
fn polluted_parent_gives_same_path_as_gui_launch() {
    let dir = zdotdir("same");
    let zdot = dir.to_str().unwrap();
    let mut user = sys::user().unwrap();
    user.shell = b"/bin/zsh".to_vec();
    let zsh = Shell::Login;

    // Terminal.app이 받는 launchd GUI 환경(실측 키). login(1)이 HOME·USER·LOGNAME·SHELL을 passwd로 채운다.
    let name = String::from_utf8(user.name.clone()).unwrap();
    let home = String::from_utf8(user.home.clone()).unwrap();
    let gui = vec![
        kv("HOME", &home),
        kv("USER", &name),
        kv("LOGNAME", &name),
        kv("SHELL", "/bin/zsh"),
        kv("PATH", "/usr/bin:/bin:/usr/sbin:/sbin"),
        kv("TERM", "xterm-256color"),
        kv("ZDOTDIR", zdot),
    ];
    let reference = zsh_path(&zsh.launch_with(None, Vec::new()).argv, &gui);

    let mut ours = zsh.launch_with(Some(&user), polluted());
    // 호출자가 세션별 값을 덧붙이는 자리(여기서는 사용자 rc 대신 테스트 rc).
    ours.env.push(kv("ZDOTDIR", zdot));
    let got = zsh_path(&ours.argv, &ours.env);

    assert!(!got.contains("/polluted"), "{got}");
    assert!(got.starts_with(&format!("{zdot}/nvm/")), "{got}");
    assert_eq!(got, reference);
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn custom_program_inherits_login_shell_rc_path() {
    let dir = std::env::temp_dir().join(format!("mk582-shell-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    let home = dir.join("home");
    let rc_bin = home.join("rc-bin");
    std::fs::create_dir_all(&rc_bin).unwrap();
    std::fs::write(home.join(".zshrc"), "export PATH=\"$HOME/rc-bin:$PATH\"\n").unwrap();
    let user = User {
        name: b"owen".to_vec(),
        home: home.as_os_str().as_bytes().to_vec(),
        shell: b"/bin/zsh".to_vec(),
    };
    let custom = Shell::Custom {
        program: b"/bin/sh".to_vec(),
        args: vec![b"-c".to_vec(), b"true".to_vec()],
    };
    let launch = custom.launch_with(Some(&user), polluted());
    let path = String::from_utf8_lossy(get(&launch, "PATH").unwrap());
    std::fs::remove_dir_all(&dir).ok();
    assert!(!path.contains("/polluted"), "{path}");
    assert!(
        path.split(':').any(|p| p == rc_bin.to_str().unwrap()),
        "custom program PATH missing rc-bin: {path}"
    );
    let cached = custom.launch();
    assert_eq!(get(&cached, "PATH"), Some(login_path()));
    assert!(std::ptr::eq(login_path(), login_path()));
}

#[test]
fn failed_login_path_probe_is_retried_and_success_is_cached() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = std::env::temp_dir().join(format!("mk595-path-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let script = dir.join("shell");
    std::fs::write(&script, "#!/bin/sh\nexit 1\n").unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
    let cache = std::sync::OnceLock::new();
    let login = || Launch {
        argv: vec![script.as_os_str().as_bytes().to_vec()],
        env: vec![],
    };
    assert_eq!(cached_path(&cache, login), LAUNCHD_PATH);
    std::fs::write(
        &script,
        "#!/bin/sh\nprintf '__MK_PATH__=/recovered/bin:/usr/bin\\n'\n",
    )
    .unwrap();
    assert_eq!(cached_path(&cache, login), b"/recovered/bin:/usr/bin");
    std::fs::remove_file(&script).unwrap();
    assert_eq!(cached_path(&cache, login), b"/recovered/bin:/usr/bin");
    std::fs::remove_dir_all(dir).unwrap();
}

/// OWE-657 감사: `Launch`의 `Debug`는 env 값을 가린다(프로필 env의 API 키 등 비밀값 로그 노출 방지).
/// 키는 보이고 값은 보이지 않아야 하며, 동등성(`PartialEq`)에는 영향이 없다.
#[test]
fn launch_debug_redacts_env_values() {
    let launch = Launch {
        argv: vec![b"/bin/zsh".to_vec()],
        env: vec![
            kv("OPENAI_API_KEY", "SECRET-secret-value"),
            kv("LANG", "ko"),
        ],
    };
    let shown = format!("{launch:?}");
    assert!(shown.contains("OPENAI_API_KEY"), "{shown}");
    assert!(!shown.contains("SECRET-secret-value"), "{shown}");
    assert!(!shown.contains(", \"ko\""), "{shown}");
    assert_eq!(
        launch,
        Launch {
            argv: vec![b"/bin/zsh".to_vec()],
            env: vec![
                kv("OPENAI_API_KEY", "SECRET-secret-value"),
                kv("LANG", "ko")
            ],
        }
    );
}
