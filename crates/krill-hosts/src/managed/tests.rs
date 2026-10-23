use super::*;
use std::os::unix::fs::PermissionsExt as _;
use std::os::unix::net::UnixListener;
use std::sync::atomic::{AtomicU64, Ordering};

static SEQ: AtomicU64 = AtomicU64::new(0);

fn test_opts() -> SshOptions {
    SshOptions::new("owen@remote.test")
}

/// 가짜 `ssh`: ssh 플래그는 무시하고 마지막 인자(원격 명령)를 로컬 `sh -c`로 실행한다.
/// stdin은 그대로 흘려 `cat >` 업로드·`/bin/sh -s` 스크립트를 실제로 수행한다.
/// `base/cfg_home`·`base/cfg_path`가 있으면 `HOME`·`PATH`를 덮어쓴다.
struct FakeSsh {
    base: PathBuf,
    home: PathBuf,
    bin: PathBuf,
}

impl FakeSsh {
    fn new(_tag: &str) -> Self {
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        // UDS 104바이트 제한: 원격 소켓(`~/Library/.../krilld.sock`)을 바인드하므로
        // `temp_dir()`(macOS는 50자 이상) 대신 `/tmp` 짧은 경로를 쓴다.
        let base = PathBuf::from(format!("/tmp/m897-{}-{seq}", std::process::id()));
        std::fs::remove_dir_all(&base).ok();
        let bin = base.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let home = base.join("home");
        std::fs::create_dir_all(&home).unwrap();
        // 원격 명령이 쓰는 도구만 가짜 bin에 심볼링크로 고정해 PATH를 격리한다.
        // 시스템에 설치된 실제 `krill`이 프로브(`command -v krill`)에 새지 않게 한다.
        for tool in [
            "sh", "dirname", "cat", "mkdir", "chmod", "id", "printf", "sleep",
        ] {
            let target = ["/bin", "/usr/bin"]
                .into_iter()
                .map(|d| format!("{d}/{tool}"))
                .find(|p| std::path::Path::new(p).is_file());
            if let Some(target) = target {
                std::os::unix::fs::symlink(&target, bin.join(tool)).ok();
            }
        }
        std::fs::write(base.join("cfg_home"), home.display().to_string()).unwrap();
        // 격리 PATH: 가짜 bin만 둔다(없는 도구는 그 테스트에서 만들지 않은 것이므로 실패가 맞다).
        std::fs::write(base.join("cfg_path"), bin.display().to_string()).unwrap();
        std::fs::write(
            base.join("ssh"),
            r#"#!/bin/sh
base=$(dirname "$0")
last=""
for a in "$@"; do last="$a"; done
printf '%s\n' "$*" >> "$base/argv"
if [ -f "$base/cfg_home" ]; then HOME=$(cat "$base/cfg_home"); export HOME; fi
if [ -f "$base/cfg_path" ]; then PATH=$(cat "$base/cfg_path"); export PATH; fi
exec sh -c "$last"
"#,
        )
        .unwrap();
        std::fs::set_permissions(base.join("ssh"), std::fs::Permissions::from_mode(0o700)).unwrap();
        Self { base, home, bin }
    }

    fn ssh_bin(&self) -> std::ffi::OsString {
        self.base.join("ssh").into_os_string()
    }

    fn argv_log(&self) -> String {
        std::fs::read_to_string(self.base.join("argv")).unwrap_or_default()
    }

    /// `uname -s`·`uname -m` 가짜.
    fn set_uname(&self, s: &str, m: &str) {
        let base = self.base.clone();
        std::fs::write(
            self.bin.join("uname"),
            format!(
                "#!/bin/sh\nbase=\"{}\"\ncase \"$1\" in -s) cat \"$base/cfg_uname_s\";; -m) cat \"$base/cfg_uname_m\";; *) echo \"$(cat \"$base/cfg_uname_s\") $(cat \"$base/cfg_uname_m\")\";; esac\n",
                base.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(
            self.bin.join("uname"),
            std::fs::Permissions::from_mode(0o700),
        )
        .unwrap();
        std::fs::write(self.base.join("cfg_uname_s"), s).unwrap();
        std::fs::write(self.base.join("cfg_uname_m"), m).unwrap();
    }

    /// `launchctl` 가짜(인자 기록, 종료 코드는 `cfg_launchctl_exit`).
    fn set_launchctl(&self, exit: i32) {
        let base = self.base.clone();
        std::fs::write(
            self.bin.join("launchctl"),
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"{}/launchctl_argv\"\nexit {}\n",
                base.display(),
                exit
            ),
        )
        .unwrap();
        std::fs::set_permissions(
            self.bin.join("launchctl"),
            std::fs::Permissions::from_mode(0o700),
        )
        .unwrap();
        std::fs::write(self.base.join("cfg_launchctl_exit"), exit.to_string()).unwrap();
    }

    fn launchctl_argv(&self) -> String {
        std::fs::read_to_string(self.base.join("launchctl_argv")).unwrap_or_default()
    }
}

impl Drop for FakeSsh {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.base).ok();
    }
}

#[test]
fn parse_os_and_arch_cover_macos_linux_and_other() {
    assert_eq!(parse_os("Darwin"), RemoteOs::Macos);
    assert_eq!(parse_os("darwin\n"), RemoteOs::Macos);
    assert_eq!(parse_os("Linux"), RemoteOs::Linux);
    assert!(matches!(parse_os("FreeBSD"), RemoteOs::Other(_)));
    assert_eq!(parse_arch("arm64"), RemoteArch::Arm64);
    assert_eq!(parse_arch("aarch64"), RemoteArch::Arm64);
    assert_eq!(parse_arch("x86_64"), RemoteArch::X86_64);
    assert_eq!(parse_arch("AMD64"), RemoteArch::X86_64);
    assert!(matches!(parse_arch("riscv64"), RemoteArch::Other(_)));

    let mac = RemotePlatform {
        os: RemoteOs::Macos,
        arch: RemoteArch::Arm64,
        kernel: "Darwin".into(),
    };
    assert!(mac.is_supported());
    assert_eq!(mac.describe(), "macOS arm64");
    assert!(mac.accepts_local_arch("aarch64"));
    assert!(!mac.accepts_local_arch("x86_64"));

    let intel = RemotePlatform {
        os: RemoteOs::Macos,
        arch: RemoteArch::X86_64,
        kernel: "Darwin".into(),
    };
    assert!(!intel.is_supported());
    assert!(!intel.accepts_local_arch("aarch64"));

    let linux = RemotePlatform {
        os: RemoteOs::Linux,
        arch: RemoteArch::X86_64,
        kernel: "Linux".into(),
    };
    assert!(!linux.is_supported());
    assert!(!linux.accepts_local_arch("x86_64"));
    assert_eq!(linux.describe(), "Linux x86_64");
}

#[test]
fn probe_command_shape_is_single_sh_c_with_labels() {
    let cmd = probe_command();
    assert!(cmd.starts_with("/bin/sh -c '"));
    for marker in [
        "krill-os=",
        "krill-arch=",
        "krill-home=",
        "krill-version=",
        "krill-sock=",
        ".krill/bin/VERSION",
        daemon::BUNDLE_ID,
    ] {
        assert!(cmd.contains(marker), "missing {marker}");
    }
    // fish·tcsh 로그인 셸에서도 같게 돌도록 /bin/sh로 감싼다.
    assert_eq!(cmd.matches("/bin/sh -c").count(), 1);
}

#[test]
fn parse_probe_covers_installed_unmanaged_missing_and_socket() {
    let full = "krill-os=Darwin\nkrill-arch=arm64\nkrill-home=/Users/remote\nkrill-version=0.1.5\nkrill-sock=dev.krill.app\n";
    let info = parse_probe(full).unwrap();
    assert_eq!(info.platform.describe(), "macOS arm64");
    assert_eq!(info.version, RemoteVersion::Known("0.1.5".into()));
    assert_eq!(info.socket_bundle.as_deref(), Some("dev.krill.app"));
    assert_eq!(
        info.socket_path(),
        PathBuf::from("/Users/remote/Library/Application Support/dev.krill.app/run/krilld.sock")
    );
    assert_eq!(info.bin_dir(), PathBuf::from("/Users/remote/.krill/bin"));

    let unmanaged =
        "krill-os=Darwin\nkrill-arch=arm64\nkrill-home=/Users/r\nkrill-version=unmanaged\n";
    let info = parse_probe(unmanaged).unwrap();
    assert_eq!(info.version, RemoteVersion::Unmanaged);
    assert!(info.socket_bundle.is_none());

    let missing = "krill-os=Linux\nkrill-arch=x86_64\nkrill-home=/home/r\nkrill-version=missing\n";
    let info = parse_probe(missing).unwrap();
    assert_eq!(info.version, RemoteVersion::Missing);
    assert_eq!(info.version_status("0.1.5"), VersionStatus::NotInstalled);

    // 버전 파일 변조(개행 삽입)는 Unmanaged으로 격하한다.
    let evil =
        "krill-os=Darwin\nkrill-arch=arm64\nkrill-home=/Users/r\nkrill-version=0.1.5; rm -rf /\n";
    assert_eq!(parse_probe(evil).unwrap().version, RemoteVersion::Unmanaged);

    assert!(parse_probe("krill-os=Darwin\n").is_err());
    assert!(
        parse_probe(
            "krill-os=Darwin\nkrill-arch=arm64\nkrill-home=relative/path\nkrill-version=missing\n"
        )
        .is_err()
    );
}

#[test]
fn version_status_compares_against_local_build() {
    let known = |v: &str| RemoteInfo {
        platform: RemotePlatform {
            os: RemoteOs::Macos,
            arch: RemoteArch::Arm64,
            kernel: "Darwin".into(),
        },
        home: "/Users/r".into(),
        version: RemoteVersion::Known(v.into()),
        socket_bundle: None,
    };
    assert_eq!(
        known("0.1.5").version_status("0.1.5"),
        VersionStatus::UpToDate
    );
    assert_eq!(
        known("0.1.4").version_status("0.1.5"),
        VersionStatus::UpdateAvailable {
            remote: "0.1.4".into(),
            local: "0.1.5".into(),
        }
    );
    // 원격이 더 새로워도 터널은 동작하므로 최신으로 본다.
    assert_eq!(
        known("0.1.6").version_status("0.1.5"),
        VersionStatus::UpToDate
    );
    assert_eq!(
        RemoteInfo {
            version: RemoteVersion::Unmanaged,
            ..known("0.1.5")
        }
        .version_status("0.1.5"),
        VersionStatus::Unknown
    );
}

#[test]
fn install_spec_rejects_bad_home_and_version() {
    assert!(InstallSpec::new("/Users/r", "0.1.5").is_ok());
    assert!(InstallSpec::new("/Users/r", "0.1.5+dev.0123abcd").is_ok());
    assert!(InstallSpec::new("relative/home", "0.1.5").is_err());
    assert!(InstallSpec::new("/Users/r:evil", "0.1.5").is_err());
    assert!(InstallSpec::new("/Users/o'brien", "0.1.5").is_err());
    assert!(InstallSpec::new("/Users/r", "").is_err());
    assert!(InstallSpec::new("/Users/r", "0.1.5; rm -rf /").is_err());
    assert!(InstallSpec::new("/Users/r", "$(evil)").is_err());
}

#[test]
fn setup_script_touches_only_install_locations() {
    let spec = InstallSpec::new("/Users/o remote", "0.1.5").unwrap();
    let script = spec.render_setup_script();
    assert!(script.contains("chmod 755"));
    assert!(script.contains("VERSION"));
    assert!(script.contains("launchctl bootstrap"));
    assert!(script.contains("bootout"));
    assert!(script.contains(HOLDER_LABEL));
    // 원격 셸 설정 파일은 절대 건드리지 않는다.
    for rc in [".zshrc", ".bashrc", ".bash_profile", ".profile", ".tcshrc"] {
        assert!(!script.contains(rc), "must not touch {rc}");
    }
    // 공백 포함 경로도 단일 인용으로 안전하게 박힌다.
    assert!(script.contains("'/Users/o remote/.krill/bin'"));

    let plist = spec.render_plist();
    assert!(plist.contains(HOLDER_LABEL));
    assert!(plist.contains("/Users/o remote/.krill/bin/krilld"));
    assert!(plist.contains("dev.krill.app/run/krilld.sock"));
}

#[test]
fn classify_remote_error_covers_guide_branches() {
    use InstallFailureKind as K;
    assert_eq!(
        classify_remote_error("No space left on device", Some(1)),
        K::DiskFull
    );
    assert_eq!(
        classify_remote_error("mkdir: /Users/r/.krill: Permission denied", Some(1)),
        K::Permission
    );
    assert_eq!(
        classify_remote_error("ssh: Could not resolve hostname bad.local", None),
        K::Network
    );
    assert_eq!(
        classify_remote_error("krill-install: unsupported OS: Linux", Some(3)),
        K::UnsupportedOs
    );
    assert_eq!(
        classify_remote_error(
            "krill-install: launchctl bootstrap failed: No such process",
            Some(6)
        ),
        K::Bootstrap
    );
    assert_eq!(classify_remote_error("weird output", Some(42)), K::Other);
}

#[test]
fn probe_roundtrip_through_fake_ssh_with_socket_and_version() {
    let fake = FakeSsh::new("probe");
    fake.set_uname("Darwin", "arm64");
    let bin = fake.home.join(".krill/bin");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::write(bin.join("VERSION"), "0.1.5\n").unwrap();
    let sock_dir = fake.home.join(format!(
        "Library/Application Support/{}/run",
        daemon::BUNDLE_ID
    ));
    std::fs::create_dir_all(&sock_dir).unwrap();
    let _sock = UnixListener::bind(sock_dir.join("krilld.sock")).unwrap();

    let info = query_remote_info_with_program(&test_opts(), &fake.ssh_bin()).unwrap();
    assert_eq!(info.platform.describe(), "macOS arm64");
    assert_eq!(info.home, fake.home.display().to_string());
    assert_eq!(info.version, RemoteVersion::Known("0.1.5".into()));
    assert_eq!(info.socket_bundle.as_deref(), Some(daemon::BUNDLE_ID));
    let log = fake.argv_log();
    assert!(log.contains("BatchMode=yes"));
    assert!(log.contains("owen@remote.test"));
}

#[test]
fn probe_roundtrip_reports_linux_as_unsupported() {
    let fake = FakeSsh::new("probelinux");
    fake.set_uname("Linux", "x86_64");
    let info = query_remote_info_with_program(&test_opts(), &fake.ssh_bin()).unwrap();
    assert!(!info.platform.is_supported());
    assert_eq!(info.platform.describe(), "Linux x86_64");
    assert_eq!(info.version, RemoteVersion::Missing);
}

#[test]
fn install_roundtrip_uploads_blobs_plist_and_bootstraps() {
    let fake = FakeSsh::new("install");
    fake.set_uname("Darwin", "arm64");
    fake.set_launchctl(0);
    let opts = test_opts();
    let home = fake.home.display().to_string();
    let spec = InstallSpec::new(&home, "0.1.5").unwrap();

    // 설치 전 프로브: 미설치 상태.
    let before = query_remote_info_with_program(&opts, &fake.ssh_bin()).unwrap();
    assert_eq!(before.version, RemoteVersion::Missing);

    let krill = b"fake-krill-binary";
    let krilld = b"fake-krilld-binary";
    let holder = b"fake-holder-binary";
    let blobs = [
        InstallBlob {
            name: "krill",
            bytes: krill,
        },
        InstallBlob {
            name: "krilld",
            bytes: krilld,
        },
        InstallBlob {
            name: "krill-holder",
            bytes: holder,
        },
    ];
    let payload = InstallPayload {
        blobs: &blobs,
        plist_xml: &spec.render_plist(),
    };
    let report =
        install_remote_with_program(&opts, &fake.ssh_bin(), &spec, "aarch64", &payload, &before)
            .unwrap();
    assert_eq!(report.version, "0.1.5");
    assert!(report.bootstrapped);

    // 원격 파일이 실제로 배치됐다.
    let bin = fake.home.join(".krill/bin");
    assert_eq!(std::fs::read(bin.join("krill")).unwrap(), krill);
    assert_eq!(std::fs::read(bin.join("krilld")).unwrap(), krilld);
    assert_eq!(std::fs::read(bin.join("krill-holder")).unwrap(), holder);
    assert_eq!(
        std::fs::read_to_string(bin.join("VERSION")).unwrap(),
        "0.1.5\n"
    );
    let plist = std::fs::read_to_string(spec.plist_path()).unwrap();
    assert!(plist.contains(HOLDER_LABEL));
    let ctl = fake.launchctl_argv();
    assert!(ctl.contains("bootstrap"));
    assert!(ctl.contains(HOLDER_LABEL));

    // 설치 후 프로브: 버전이 보인다.
    let after = query_remote_info_with_program(&opts, &fake.ssh_bin()).unwrap();
    assert_eq!(after.version, RemoteVersion::Known("0.1.5".into()));
}

#[test]
fn install_refuses_unsupported_platform_and_arch_mismatch() {
    let fake = FakeSsh::new("refuse");
    fake.set_uname("Linux", "x86_64");
    let opts = test_opts();
    let home = fake.home.display().to_string();
    let spec = InstallSpec::new(&home, "0.1.5").unwrap();
    let remote = query_remote_info_with_program(&opts, &fake.ssh_bin()).unwrap();
    let blobs = [InstallBlob {
        name: "krill",
        bytes: b"x",
    }];
    let payload = InstallPayload {
        blobs: &blobs,
        plist_xml: &spec.render_plist(),
    };
    let err =
        install_remote_with_program(&opts, &fake.ssh_bin(), &spec, "aarch64", &payload, &remote)
            .unwrap_err();
    assert_eq!(err.kind, InstallFailureKind::UnsupportedOs);

    fake.set_uname("Darwin", "x86_64");
    let remote = query_remote_info_with_program(&opts, &fake.ssh_bin()).unwrap();
    let err =
        install_remote_with_program(&opts, &fake.ssh_bin(), &spec, "aarch64", &payload, &remote)
            .unwrap_err();
    assert_eq!(err.kind, InstallFailureKind::UnsupportedArch);
}

#[test]
fn install_reports_bootstrap_failure_with_guidance_kind() {
    let fake = FakeSsh::new("bootfail");
    fake.set_uname("Darwin", "arm64");
    fake.set_launchctl(1);
    let opts = test_opts();
    let home = fake.home.display().to_string();
    let spec = InstallSpec::new(&home, "0.1.5").unwrap();
    let remote = query_remote_info_with_program(&opts, &fake.ssh_bin()).unwrap();
    // 설정 스크립트가 세 바이너리를 모두 요구하므로 전부 업로드해야 bootstrap 단계에 닿는다.
    let blobs = [
        InstallBlob {
            name: "krill",
            bytes: b"x",
        },
        InstallBlob {
            name: "krilld",
            bytes: b"x",
        },
        InstallBlob {
            name: "krill-holder",
            bytes: b"x",
        },
    ];
    let payload = InstallPayload {
        blobs: &blobs,
        plist_xml: &spec.render_plist(),
    };
    let err =
        install_remote_with_program(&opts, &fake.ssh_bin(), &spec, "aarch64", &payload, &remote)
            .unwrap_err();
    assert_eq!(err.kind, InstallFailureKind::Bootstrap);
    assert_eq!(err.step, "setup");
}
