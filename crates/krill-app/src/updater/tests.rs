use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt as _;
use std::process::Command;
use std::thread;

use gpui::{TestAppContext, VisualTestContext};
use ring::signature::{Ed25519KeyPair, KeyPair as _};

use super::ed25519::{decode_hex, encode_hex, sha256_hex, verify};
use super::relaunch::text;
use super::*;

fn test_public_key(secret: &[u8; 32]) -> PublicKey {
    Ed25519KeyPair::from_seed_unchecked(secret)
        .unwrap()
        .public_key()
        .as_ref()
        .try_into()
        .unwrap()
}

fn sign_manifest(m: &mut Manifest, secret: &[u8; 32]) {
    let kp = Ed25519KeyPair::from_seed_unchecked(secret).unwrap();
    let sig = kp.sign(&m.signing_payload());
    m.ed25519_sig = encode_hex(sig.as_ref());
}

/// updater.toml에 넣을 수 있는 fresh 키(RFC 8032 테스트 키는 설정에서 금지된다).
fn fresh_key() -> (Ed25519KeyPair, PublicKey) {
    use ring::rand::SecureRandom as _;
    let mut seed = [0u8; 32];
    ring::rand::SystemRandom::new().fill(&mut seed).unwrap();
    let kp = Ed25519KeyPair::from_seed_unchecked(&seed).unwrap();
    let pk: PublicKey = kp.public_key().as_ref().try_into().unwrap();
    (kp, pk)
}

/// fresh 키로 매니페스트에 서명한다.
fn sign_manifest_with(kp: &Ed25519KeyPair, m: &mut Manifest) {
    let sig = kp.sign(&m.signing_payload());
    m.ed25519_sig = encode_hex(sig.as_ref());
}

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("mk-updater-{}-{name}", std::process::id()));
    fs::remove_dir_all(&dir).ok();
    fs::create_dir_all(&dir).unwrap();
    dir
}

struct TempGuard(PathBuf);

impl Drop for TempGuard {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).ok();
    }
}

/// 테스트용 `.app` 번들을 만들고, `sign_bundle`이 참이면 번들을 `codesign -s -`로 서명한다.
fn build_test_app_bundle(bundle: &Path, version: &str, body_script: &str, sign_bundle: bool) {
    let macos = bundle.join("Contents/MacOS");
    let resources = bundle.join("Contents/Resources");
    fs::create_dir_all(&macos).unwrap();
    fs::create_dir_all(&resources).unwrap();
    let plist = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>CFBundleIdentifier</key>
	<string>dev.krill.app</string>
	<key>CFBundleExecutable</key>
	<string>krill-app</string>
	<key>CFBundleName</key>
	<string>Krill</string>
	<key>CFBundlePackageType</key>
	<string>APPL</string>
	<key>CFBundleShortVersionString</key>
	<string>{version}</string>
	<key>CFBundleVersion</key>
	<string>{version}</string>
</dict>
</plist>
"#
    );
    fs::write(bundle.join("Contents/Info.plist"), plist).unwrap();
    fs::write(resources.join("version.txt"), version).unwrap();
    let exe = macos.join("krill-app");
    fs::write(&exe, format!("#!/bin/sh\n{body_script}\n")).unwrap();
    fs::set_permissions(&exe, fs::Permissions::from_mode(0o755)).unwrap();

    if sign_bundle {
        let s = Command::new("/usr/bin/codesign")
            .args(["--force", "-s", "-", "--timestamp=none"])
            .arg(bundle)
            .status()
            .unwrap();
        assert!(s.success());
    }
}

/// `.app` 번들을 `ditto -c -k --sequesterRsrc --keepParent`로 zip 아카이브 바이트로 만든다.
fn zip_bundle(bundle: &Path, zip_path: &Path) -> Vec<u8> {
    let status = Command::new("/usr/bin/ditto")
        .args(["-c", "-k", "--sequesterRsrc", "--keepParent"])
        .arg(bundle)
        .arg(zip_path)
        .status()
        .unwrap();
    assert!(status.success());
    fs::read(zip_path).unwrap()
}

fn bundle_version(bundle: &Path) -> String {
    fs::read_to_string(bundle.join("Contents/Resources/version.txt")).unwrap()
}

#[test]
fn rfc8032_vectors_and_ed25519_tamper_rejection() {
    // RFC 8032 §7.1 TEST 1 (빈 메시지)
    let sk1 = decode_hex::<32>("9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60")
        .unwrap();
    let pk1 = decode_hex::<32>("d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a")
        .unwrap();
    let sig1 = decode_hex::<64>(
        "e5564300c360ac729086e2cc806e828a84877f1eb8e5d974d873e06522490155\
         5fb8821590a33bacc61e39701cf9b46bd25bf5f0595bbe24655141438e7a100b",
    )
    .unwrap();
    assert_eq!(test_public_key(&sk1), pk1);
    assert!(verify(&pk1, b"", &sig1));

    // RFC 8032 §7.1 TEST 2 (1바이트 0x72)
    let sk2 = decode_hex::<32>("4ccd089b28ff96da9db6c346ec114e0f5b8a319f35aba624da8cf6ed4fb8a6fb")
        .unwrap();
    let pk2 = decode_hex::<32>("3d4017c3e843895a92b70aa74d1b7ebc9c982ccf2ec4968cc0cd55f12af4660c")
        .unwrap();
    let sig2 = decode_hex::<64>(
        "92a009a9f0d4cab8720e820b5f642540a2b27b5416503f8fb3762223ebdb69da\
         085ac1e43e15996e458f3613d0f11d8c387b2eaeb4302aeeb00d291612bb0c00",
    )
    .unwrap();
    assert_eq!(test_public_key(&sk2), pk2);
    assert!(verify(&pk2, &[0x72], &sig2));

    // RFC 8032 §7.1 TEST 3 (2바이트 0xaf82)
    let sk3 = decode_hex::<32>("c5aa8df43f9f837bedb7442f31dcb7b166d38535076f094b85ce3a2e0b4458f7")
        .unwrap();
    let pk3 = decode_hex::<32>("fc51cd8e6218a1a38da47ed00230f0580816ed13ba3303ac5deb911548908025")
        .unwrap();
    let sig3 = decode_hex::<64>(
        "6291d657deec24024827e69c3abe01a30ce548a284743a445e3680d7db5ac3ac\
         18ff9b538d16f290ae67f760984dc6594a7c15e9716ed28dc027beceea1ec40a",
    )
    .unwrap();
    assert_eq!(test_public_key(&sk3), pk3);
    assert!(verify(&pk3, &[0xaf, 0x82], &sig3));

    // 메시지·서명·공개키 1비트 변조 거부
    assert!(!verify(&pk3, &[0xaf, 0x83], &sig3));
    assert!(!verify(&pk1, &[0xaf, 0x82], &sig3));
    let mut bad_r = sig3;
    bad_r[0] ^= 1;
    assert!(!verify(&pk3, &[0xaf, 0x82], &bad_r));
    let mut bad_s = sig3;
    bad_s[40] ^= 1;
    assert!(!verify(&pk3, &[0xaf, 0x82], &bad_s));

    // S >= L 비정규 스칼라 거부(RFC 8032 §5.1.7)
    let mut non_canonical = sig1;
    non_canonical[32..].copy_from_slice(&[0xff; 32]);
    assert!(!verify(&pk1, b"", &non_canonical));
}

#[test]
fn manifest_parsing_signature_downgrade_channel_rollout_and_url() {
    let sk = decode_hex::<32>("9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60")
        .unwrap();
    let pk = test_public_key(&sk);
    let other_pk =
        decode_hex::<32>("3d4017c3e843895a92b70aa74d1b7ebc9c982ccf2ec4968cc0cd55f12af4660c")
            .unwrap();

    let mut m = Manifest {
        version: "0.2.0".to_owned(),
        url: "https://github.com/craftowen/krill/releases/download/v0.2.0/Krill-0.2.0.zip"
            .to_owned(),
        sha256: sha256_hex(b"archive-bytes"),
        ed25519_sig: String::new(),
        min_daemon_protocol: 1,
        channel: Channel::Stable,
        rollout_pct: 50,
    };
    sign_manifest(&mut m, &sk);

    let json = serde_json::to_string(&m).unwrap();
    let parsed = Manifest::parse(&json).unwrap();
    assert_eq!(parsed, m);
    assert_eq!(
        parsed
            .evaluate(&pk, "0.1.0", Channel::Stable, PROTO_MAX, 10)
            .unwrap(),
        Decision::Available
    );
    assert_eq!(
        parsed
            .evaluate(&pk, "0.1.0", Channel::Stable, PROTO_MAX, 50)
            .unwrap(),
        Decision::DeferredByRollout
    );
    assert_eq!(
        parsed
            .evaluate(&pk, "0.1.0", Channel::Beta, PROTO_MAX, 10)
            .unwrap(),
        Decision::ChannelMismatch
    );
    assert_eq!(
        parsed
            .evaluate(&pk, "0.2.0", Channel::Stable, PROTO_MAX, 10)
            .unwrap(),
        Decision::UpToDate
    );
    assert!(matches!(
        parsed.evaluate(&pk, "0.3.0", Channel::Stable, PROTO_MAX, 10),
        Err(Error::Downgrade { .. })
    ));
    assert!(matches!(
        parsed.evaluate(&other_pk, "0.1.0", Channel::Stable, PROTO_MAX, 10),
        Err(Error::BadSignature)
    ));

    // 서명 뒤 필드 변조는 모두 거부
    let mut tampered_ver = parsed.clone();
    tampered_ver.version = "0.2.1".to_owned();
    assert!(matches!(
        tampered_ver.verify_signature(&pk),
        Err(Error::BadSignature)
    ));
    let mut tampered_sha = parsed.clone();
    tampered_sha.sha256 = sha256_hex(b"other");
    assert!(matches!(
        tampered_sha.verify_signature(&pk),
        Err(Error::BadSignature)
    ));

    // AS-12: 매니페스트·설정은 다시 직렬화하지 않으므로 미지 필드를 무시하고
    // 서명 v1에 포함되지 않은 미래 정책 필드는 엄격히 거부한다.
    let unknown = format!(
        "{{\"version\":\"0.2.0\",\"url\":\"https://github.com/craftowen/krill/releases/download/v0.2.0/a.zip\",\"sha256\":\"{}\",\"ed25519_sig\":\"{}\",\"min_daemon_protocol\":1,\"channel\":\"stable\",\"rollout_pct\":100,\"extra\":1}}",
        "00".repeat(32),
        "00".repeat(64)
    );
    assert!(Manifest::parse(&unknown).is_err());

    // 원격 평문 HTTP·file 스킴 거부, HTTPS·로컬 루프백 HTTP만 허용
    assert!(
        validate_url("https://github.com/craftowen/krill/releases/latest/download/latest.json")
            .is_ok()
    );
    assert!(validate_url("http://127.0.0.1:8080/latest.json").is_ok());
    assert!(validate_url("http://localhost:9000/latest.json").is_ok());
    assert!(matches!(
        validate_url("http://github.com/craftowen/krill/releases/latest/download/latest.json"),
        Err(Error::InsecureUrl(_))
    ));
    assert!(matches!(
        validate_url("file:///tmp/latest.json"),
        Err(Error::InsecureUrl(_))
    ));

    // updater.toml 설정 파싱
    let pk = test_public_key(&[42; 32]);
    let cfg = Config::parse(&format!(
        "url = \"http://127.0.0.1:4242/latest.json\"\nchannel = \"beta\"\npublic_key = \"{}\"\ndesignated_requirement = \"anchor apple generic\"\n",
        encode_hex(&pk)
    ))
    .unwrap();
    assert_eq!(cfg.channel, Channel::Beta);
    assert_eq!(cfg.public_key_bytes().unwrap(), pk);
    assert!(Config::parse("url = \"http://evil.com/latest.json\"").is_err());
    // AS-12: updater.toml 미지 필드도 무시한다(다운그레이드 잠금 방지).
    let cfg_extra = Config::parse(&format!(
        "url = \"http://127.0.0.1:4242/latest.json\"\npublic_key = \"{}\"\nfuture_option = 1\n",
        encode_hex(&pk)
    ))
    .expect("config unknown fields must be ignored");
    assert_eq!(cfg_extra.channel, Channel::Stable);
}

#[test]
fn channel_mismatch_surfaces_configured_and_offered_without_staging() {
    // NOTE: RFC 8032 테스트 벡터 키는 public_key_bytes()가 거부하므로 [42; 32]를 쓴다.
    let sk = [42; 32];
    let pk = test_public_key(&sk);
    let mut m = Manifest {
        version: "0.2.0".to_owned(),
        url: "https://github.com/craftowen/krill/releases/download/v0.2.0/Krill-0.2.0.zip"
            .to_owned(),
        sha256: sha256_hex(b"archive-bytes"),
        ed25519_sig: String::new(),
        min_daemon_protocol: 1,
        channel: Channel::Beta,
        rollout_pct: 100,
    };
    sign_manifest(&mut m, &sk);
    let cfg = Config {
        channel: Channel::Stable,
        public_key: encode_hex(&pk),
        ..Config::default()
    };
    let d = TempGuard(temp_dir("channel-mismatch"));
    let app = d.0.join("Krill.app");
    let status = decide_and_stage(&cfg, &m, &app, &d.0).unwrap();
    assert!(matches!(
        status,
        CheckStatus::ChannelMismatch {
            configured: Channel::Stable,
            offered: Channel::Beta
        }
    ));
    assert!(!app.exists(), "a mismatched channel must not stage an app");
}

#[test]
fn newer_release_skipped_by_rollout_or_protocol_never_reports_up_to_date() {
    let sk = [42; 32];
    let cfg = Config {
        public_key: encode_hex(&test_public_key(&sk)),
        ..Config::default()
    };
    let d = TempGuard(temp_dir("not-up-to-date"));
    let app = d.0.join("Krill.app");
    build_test_app_bundle(&app, "0.2.6", "exit 0", false);
    let mut manifest = Manifest {
        version: "0.2.7".into(),
        url: "https://example.com/never-download.zip".into(),
        sha256: sha256_hex(b"never-download"),
        ed25519_sig: String::new(),
        min_daemon_protocol: 1,
        channel: Channel::Stable,
        rollout_pct: 0,
    };
    sign_manifest(&mut manifest, &sk);
    let CheckStatus::Error(message) = decide_and_stage(&cfg, &manifest, &app, &d.0).unwrap() else {
        panic!("a newer release outside rollout must not appear up to date");
    };
    assert_eq!(
        message,
        crate::i18n::catalog::updater_relaunch::UPDATE_ERR_ROLLOUT.format(&[("pct", "0")])
    );
    manifest.rollout_pct = 100;
    manifest.min_daemon_protocol = PROTO_MAX.checked_add(1).unwrap();
    sign_manifest(&mut manifest, &sk);
    let CheckStatus::Error(message) = decide_and_stage(&cfg, &manifest, &app, &d.0).unwrap() else {
        panic!("a newer incompatible release must not appear up to date");
    };
    assert!(message.contains("0.2.7"));
    assert!(!staging_path(&app).exists());
    for decision in [
        Decision::ChannelMismatch,
        Decision::DeferredByRollout,
        Decision::IncompatibleProtocol {
            required: PROTO_MAX + 1,
            supported: PROTO_MAX,
        },
    ] {
        assert_ne!(decision.cli_status(), "up_to_date");
        assert!(
            !decision
                .skip_message("0.2.6", &manifest, cfg.channel)
                .contains("up to date")
        );
    }
}

#[test]
fn forged_archive_and_codesign_verification_interface() {
    let d = TempGuard(temp_dir("codesign"));
    let sk = decode_hex::<32>("9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60")
        .unwrap();
    let pk = test_public_key(&sk);

    let target_app = d.0.join("Krill.app");
    build_test_app_bundle(&target_app, "0.1.0", "exit 0", true);

    // 1) 미서명 번들 아카이브: Ed25519와 SHA-256이 맞아도 codesign 검증에서 거부되고 staging이 지워진다.
    let unsigned_src = d.0.join("src-unsigned/Krill.app");
    build_test_app_bundle(&unsigned_src, "0.2.0", "exit 0", false);
    let unsigned_zip = zip_bundle(&unsigned_src, &d.0.join("unsigned.zip"));
    let mut m_unsigned = Manifest {
        version: "0.2.0".to_owned(),
        url: "https://github.com/craftowen/krill/releases/download/v0.2.0/Krill-0.2.0.zip"
            .to_owned(),
        sha256: sha256_hex(&unsigned_zip),
        ed25519_sig: String::new(),
        min_daemon_protocol: 1,
        channel: Channel::Stable,
        rollout_pct: 100,
    };
    sign_manifest(&mut m_unsigned, &sk);
    let res = stage_verified_archive(
        &m_unsigned,
        &pk,
        &unsigned_zip,
        &target_app,
        &CodesignVerifier::adhoc(),
    );
    assert!(matches!(res, Err(Error::Codesign(_))), "{res:?}");
    assert!(!staging_path(&target_app).exists());
    assert_eq!(bundle_version(&target_app), "0.1.0");

    // 2) 서명 뒤 실행 파일이 변조된 번들: codesign --verify --strict가 거부한다.
    let tampered_src = d.0.join("src-tampered/Krill.app");
    build_test_app_bundle(&tampered_src, "0.2.0", "exit 0", true);
    fs::write(
        tampered_src.join("Contents/MacOS/krill-app"),
        "#!/bin/sh\necho pwned\n",
    )
    .unwrap();
    let tampered_zip = zip_bundle(&tampered_src, &d.0.join("tampered.zip"));
    let mut m_tampered = m_unsigned.clone();
    m_tampered.sha256 = sha256_hex(&tampered_zip);
    sign_manifest(&mut m_tampered, &sk);
    let res = stage_verified_archive(
        &m_tampered,
        &pk,
        &tampered_zip,
        &target_app,
        &CodesignVerifier::adhoc(),
    );
    assert!(matches!(res, Err(Error::Codesign(_))), "{res:?}");
    assert!(!staging_path(&target_app).exists());

    // 3) 정상 ad-hoc 서명 번들: SHA-256이 틀리면 거부, 맞으면 adhoc 검증 통과,
    //    Developer ID requirement가 켜지면(MK-104 이후) ad-hoc 서명을 거부한다.
    let valid_src = d.0.join("src-valid/Krill.app");
    build_test_app_bundle(&valid_src, "0.2.0", "exit 0", true);
    let valid_zip = zip_bundle(&valid_src, &d.0.join("valid.zip"));
    let mut m_valid = m_unsigned;
    m_valid.sha256 = sha256_hex(&valid_zip);
    sign_manifest(&mut m_valid, &sk);

    let mut corrupted_zip = valid_zip.clone();
    corrupted_zip[0] ^= 0xff;
    assert!(matches!(
        stage_verified_archive(
            &m_valid,
            &pk,
            &corrupted_zip,
            &target_app,
            &CodesignVerifier::adhoc(),
        ),
        Err(Error::Sha256Mismatch { .. })
    ));

    let req_verifier = CodesignVerifier::with_requirement(
        "anchor apple generic and certificate leaf[subject.OU] = \"TEAMID1234\"",
    );
    assert!(matches!(
        stage_verified_archive(&m_valid, &pk, &valid_zip, &target_app, &req_verifier),
        Err(Error::Codesign(_))
    ));
    assert!(!staging_path(&target_app).exists());

    let staged = stage_verified_archive(
        &m_valid,
        &pk,
        &valid_zip,
        &target_app,
        &CodesignVerifier::adhoc(),
    )
    .unwrap();
    assert_eq!(bundle_version(&staged), "0.2.0");
}

#[test]
fn journal_crash_recovery_restores_previous_bundle() {
    let d = TempGuard(temp_dir("journal"));
    let state = d.0.join("state");
    let app = d.0.join("Krill.app");
    let prev = prev_path(&app);
    build_test_app_bundle(&prev, "0.1.0", "exit 0", true);

    let j = Journal {
        bundle: app.clone(),
        from_version: "0.1.0".to_owned(),
        to_version: "0.2.0".to_owned(),
        health_timeout_ms: 5_000,
        stage: Stage::MovingStagingToTarget,
    };
    j.save(&state).unwrap();

    let recovered = recover_journal(&state).unwrap().unwrap();
    assert!(matches!(recovered.stage, Stage::RolledBack { .. }));
    assert!(app.exists());
    assert_eq!(bundle_version(&app), "0.1.0");
}

fn open_window<'a>(cx: &'a mut TestAppContext, state: &Path) -> &'a mut VisualTestContext {
    let (_, cx) = cx.add_window_view(|_, _| gpui::Empty);
    let state = state.to_path_buf();
    cx.update(|window, cx| offer_rollback(state, window, cx));
    cx.run_until_parked();
    cx
}

#[gpui::test]
fn gpui_rollback_dialogs_confirm_and_restore(cx: &mut TestAppContext) {
    let d = TempGuard(temp_dir("gpui-rollback"));
    let state = d.0.join("state");
    let app = d.0.join("Krill.app");
    let prev = prev_path(&app);
    build_test_app_bundle(&app, "0.2.0", "exit 1", true);
    build_test_app_bundle(&prev, "0.1.0", "exit 0", true);

    // AS-10: Stage::RollbackNeeded(수동 확인 대기)는 생성자가 없는 죽은 경로라
    // 제거했다. 자동 롤백 안내(RolledBack) 대화상자만 남는다.
    // RolledBack 상태: 자동 롤백 안내 대화상자를 띄우고 확인 뒤 다시 묻지 않는다.
    let j2 = Journal {
        bundle: app.clone(),
        from_version: "0.1.0".to_owned(),
        to_version: "0.2.0".to_owned(),
        health_timeout_ms: 1_000,
        stage: Stage::RolledBack {
            reason: "exited with code 1".to_owned(),
        },
    };
    j2.save(&state).unwrap();
    let vcx = open_window(cx, &state);
    vcx.run_until_parked();
    let (title, _) = vcx.pending_prompt().expect("rollback notice prompt");
    assert_eq!(title, text::ROLLBACK_DONE_TITLE.get());
    vcx.simulate_prompt_answer(text::OK_BUTTON.get());
    vcx.run_until_parked();
    assert_eq!(
        Journal::load(&state).unwrap().unwrap().stage,
        Stage::Acknowledged
    );
}

#[test]
fn local_http_update_relaunch_and_failed_relaunch_rollback() {
    let d = TempGuard(temp_dir("http-e2e"));
    let state = d.0.join("state");
    let app = d.0.join("Krill.app");

    let sk = decode_hex::<32>("9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60")
        .unwrap();
    let pk = test_public_key(&sk);

    // v0.1.0 초기 번들
    let v1_marker = d.0.join("ran-0.1.0");
    build_test_app_bundle(
        &app,
        "0.1.0",
        &format!(
            "[ -n \"${READY_ENV}\" ] && echo ok > \"${READY_ENV}\"\necho 0.1.0 > '{}'",
            v1_marker.display()
        ),
        true,
    );

    // v0.2.0 정상 번들(재실행 시 READY_ENV 신호를 쓰고 마커를 남긴다)
    let v2_src = d.0.join("src-0.2.0/Krill.app");
    let v2_marker = d.0.join("ran-0.2.0");
    build_test_app_bundle(
        &v2_src,
        "0.2.0",
        &format!(
            "[ -n \"${READY_ENV}\" ] && echo ok > \"${READY_ENV}\"\necho 0.2.0 >> '{}'",
            v2_marker.display()
        ),
        true,
    );
    let v2_zip = zip_bundle(&v2_src, &d.0.join("v2.zip"));

    // v0.3.0 고장 번들(재실행 직후 비정상 종료해 ready 신호를 보내지 못함)
    let v3_src = d.0.join("src-0.3.0/Krill.app");
    build_test_app_bundle(&v3_src, "0.3.0", "exit 42", true);
    let v3_zip = zip_bundle(&v3_src, &d.0.join("v3.zip"));

    // 로컬 HTTP 서버 기동
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let base_url = format!("http://127.0.0.1:{}", addr.port());

    let mut m_v2 = Manifest {
        version: "0.2.0".to_owned(),
        url: format!("{base_url}/Krill-0.2.0.zip"),
        sha256: sha256_hex(&v2_zip),
        ed25519_sig: String::new(),
        min_daemon_protocol: 1,
        channel: Channel::Stable,
        rollout_pct: 100,
    };
    sign_manifest(&mut m_v2, &sk);
    let m_v2_json = serde_json::to_vec(&m_v2).unwrap();

    let mut m_v3 = Manifest {
        version: "0.3.0".to_owned(),
        url: format!("{base_url}/Krill-0.3.0.zip"),
        sha256: sha256_hex(&v3_zip),
        ed25519_sig: String::new(),
        min_daemon_protocol: 1,
        channel: Channel::Stable,
        rollout_pct: 100,
    };
    sign_manifest(&mut m_v3, &sk);
    let m_v3_json = serde_json::to_vec(&m_v3).unwrap();

    let server = thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let mut stream = stream;
            let mut buf = [0u8; 1024];
            let n = stream.read(&mut buf).unwrap_or(0);
            let req = String::from_utf8_lossy(&buf[..n]);
            let path = req
                .lines()
                .next()
                .and_then(|l| l.split_whitespace().nth(1))
                .unwrap_or("/");
            let body: &[u8] = match path {
                "/v2/latest.json" => &m_v2_json,
                "/Krill-0.2.0.zip" => &v2_zip,
                "/v3/latest.json" => &m_v3_json,
                "/Krill-0.3.0.zip" => &v3_zip,
                "/stop" => break,
                _ => b"not found",
            };
            let header = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            stream.write_all(header.as_bytes()).ok();
            stream.write_all(body).ok();
        }
    });

    // 1) 로컬 HTTP에서 v0.2.0 다운로드·검증·교체·재실행 성공
    let fetched_v2 = fetch_manifest(&format!("{base_url}/v2/latest.json")).unwrap();
    let staged_v2 = download_and_stage(
        &fetched_v2,
        &pk,
        "0.1.0",
        Channel::Stable,
        &app,
        &CodesignVerifier::adhoc(),
        0,
    )
    .unwrap();
    assert!(staged_v2.is_some());

    let outcome = relaunch_and_verify(
        &app,
        &state,
        "0.1.0",
        "0.2.0",
        None,
        Duration::from_secs(20),
        &CodesignVerifier::adhoc(),
    )
    .unwrap();
    match outcome {
        RelaunchOutcome::Updated { mut child } => {
            child.wait().ok();
        }
        RelaunchOutcome::RolledBack { reason, .. } => unreachable!("unexpected rollback: {reason}"),
    }
    assert_eq!(bundle_version(&app), "0.2.0");
    assert_eq!(bundle_version(&prev_path(&app)), "0.1.0");
    assert_eq!(
        Journal::load(&state).unwrap().unwrap().stage,
        Stage::Healthy
    );

    // 2) 로컬 HTTP에서 고장 난 v0.3.0 다운로드·교체 → 재실행 실패 감지 → v0.2.0으로 자동 롤백·복구 실행
    let fetched_v3 = fetch_manifest(&format!("{base_url}/v3/latest.json")).unwrap();
    let staged_v3 = download_and_stage(
        &fetched_v3,
        &pk,
        "0.2.0",
        Channel::Stable,
        &app,
        &CodesignVerifier::adhoc(),
        0,
    )
    .unwrap();
    assert!(staged_v3.is_some());

    let outcome_v3 = relaunch_and_verify(
        &app,
        &state,
        "0.2.0",
        "0.3.0",
        None,
        Duration::from_millis(500),
        &CodesignVerifier::adhoc(),
    )
    .unwrap();
    match outcome_v3 {
        RelaunchOutcome::RolledBack {
            reason,
            mut restored,
        } => {
            assert!(reason.contains("exited before ready"), "{reason}");
            restored.wait().ok();
        }
        RelaunchOutcome::Updated { .. } => unreachable!("broken v0.3.0 must roll back"),
    }
    assert_eq!(bundle_version(&app), "0.2.0");
    assert!(matches!(
        Journal::load(&state).unwrap().unwrap().stage,
        Stage::RolledBack { .. }
    ));

    fetch_bytes(&format!("{base_url}/stop"), 64).ok();
    server.join().ok();
}

#[test]
fn regression_updater_missing_key_is_disabled() {
    let dir = temp_dir("missing-key");
    let _guard = TempGuard(dir.clone());
    assert!(Config::load(&dir).unwrap().public_key_bytes().is_err());
    assert!(Config::parse("").unwrap().public_key_bytes().is_err());
}

#[test]
fn regression_updater_rejects_rfc8032_key() {
    for key in TEST_PUBLIC_KEYS {
        assert!(Config::parse(&format!("public_key = \"{key}\"")).is_err());
        let cfg = Config {
            public_key: key.to_ascii_uppercase(),
            ..Config::default()
        };
        assert!(cfg.public_key_bytes().is_err());
    }
}

#[derive(Debug)]
struct RemoveStaging;
impl CodeVerifier for RemoveStaging {
    fn verify(&self, app: &Path) -> Result<(), Error> {
        fs::remove_dir_all(app)?;
        Ok(())
    }
}

#[test]
fn regression_relauncher_restarts_original_after_rename_failure() {
    let d = TempGuard(temp_dir("rename-failure"));
    let app = d.0.join("Krill.app");
    build_test_app_bundle(&app, "0.1.0", "exit 0", false);
    build_test_app_bundle(&staging_path(&app), "0.2.0", "exit 0", false);
    let result = relaunch_and_verify(
        &app,
        &d.0.join("state"),
        "0.1.0",
        "0.2.0",
        None,
        Duration::from_millis(100),
        &RemoveStaging,
    );
    let RelaunchOutcome::RolledBack { mut restored, .. } = result.unwrap() else {
        panic!("original bundle must restart");
    };
    assert!(restored.wait().unwrap().success());
    assert_eq!(bundle_version(&app), "0.1.0");
}

#[gpui::test]
fn regression_startup_does_not_report_ready_before_attached(cx: &mut TestAppContext) {
    let d = TempGuard(temp_dir("not-attached"));
    let (_, vcx) = cx.add_window_view(|_, _| gpui::Empty);
    vcx.update(|window, cx| on_startup(d.0.clone(), window, cx));
    assert!(!ready_path(&d.0, env!("CARGO_PKG_VERSION")).exists());
}

#[test]
fn regression_http_redirect_is_refused() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    let location = url.clone();
    let (stop, stopped) = std::sync::mpsc::channel();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut buf = [0; 1024];
        assert!(stream.read(&mut buf).unwrap() > 0);
        write!(stream, "HTTP/1.1 302 Found\r\nLocation: {location}final\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
        drop(stream);
        listener.set_nonblocking(true).unwrap();
        loop {
            if let Ok((mut stream, _)) = listener.accept() {
                assert!(stream.read(&mut buf).unwrap() > 0);
                stream
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok",
                    )
                    .unwrap();
                break;
            }
            if stopped.try_recv().is_ok() {
                break;
            }
            thread::sleep(Duration::from_millis(5));
        }
    });
    let result = fetch_bytes(&url, 1024);
    stop.send(()).ok();
    server.join().unwrap();
    assert!(matches!(result, Err(Error::Http(_))), "{result:?}");
}

#[test]
fn regression_live_old_app_aborts_replacement() {
    #[derive(Debug)]
    struct Accept;
    impl CodeVerifier for Accept {
        fn verify(&self, _: &Path) -> Result<(), Error> {
            Ok(())
        }
    }
    let d = TempGuard(temp_dir("live-old-app"));
    let app = d.0.join("Krill.app");
    build_test_app_bundle(&app, "0.1.0", "exit 0", false);
    build_test_app_bundle(
        &staging_path(&app),
        "0.2.0",
        "echo ok > \"$KRILL_UPDATE_READY_FILE\"",
        false,
    );
    let result = relaunch_and_verify(
        &app,
        &d.0.join("state"),
        "0.1.0",
        "0.2.0",
        Some(std::process::id()),
        Duration::from_secs(1),
        &Accept,
    );
    if let Ok(RelaunchOutcome::Updated { mut child }) = result {
        child.wait().unwrap();
    }
    assert_eq!(bundle_version(&app), "0.1.0");
    assert!(staging_path(&app).exists());
}

#[test]
fn bundle_layout_accepts_helpers_and_rejects_broken_bundles() {
    let d = TempGuard(temp_dir("bundle-layout"));
    // Contents/Helpers 배치: 헬퍼는 Helpers에서, krill·krill-app은 MacOS에서 찾는다.
    let bundle = d.0.join("Krill.app");
    build_test_app_bundle(&bundle, "0.2.0", "exit 0", false);
    fs::create_dir_all(bundle.join("Contents/Helpers")).unwrap();
    for helper in ["krill-holder", "krilld", "krill-link"] {
        fs::write(
            bundle.join("Contents/Helpers").join(helper),
            "#!/bin/sh\nexit 0\n",
        )
        .unwrap();
    }
    fs::write(bundle.join("Contents/MacOS/krill"), "#!/bin/sh\nexit 0\n").unwrap();
    assert_eq!(
        validate_bundle_layout(&bundle).unwrap(),
        vec![
            bundle.join("Contents/Helpers/krill-holder"),
            bundle.join("Contents/Helpers/krilld"),
            bundle.join("Contents/Helpers/krill-link"),
            bundle.join("Contents/MacOS/krill"),
            bundle.join("Contents/MacOS/krill-app"),
        ]
    );

    // 구 배치(MacOS 직속 헬퍼)도 받아준다.
    let legacy = d.0.join("Legacy.app");
    build_test_app_bundle(&legacy, "0.2.0", "exit 0", false);
    for bin in ["krill-holder", "krilld", "krill-link", "krill"] {
        fs::write(
            legacy.join("Contents/MacOS").join(bin),
            "#!/bin/sh\nexit 0\n",
        )
        .unwrap();
    }
    assert_eq!(
        validate_bundle_layout(&legacy).unwrap(),
        vec![
            legacy.join("Contents/MacOS/krill-holder"),
            legacy.join("Contents/MacOS/krilld"),
            legacy.join("Contents/MacOS/krill-link"),
            legacy.join("Contents/MacOS/krill"),
            legacy.join("Contents/MacOS/krill-app"),
        ]
    );

    // Info.plist·주 실행 파일이 없으면 번들 구조 오류.
    fs::remove_file(bundle.join("Contents/Info.plist")).unwrap();
    assert!(matches!(
        validate_bundle_layout(&bundle),
        Err(Error::BundleStructure(_))
    ));
    build_test_app_bundle(&bundle, "0.2.0", "exit 0", false);
    fs::remove_file(bundle.join("Contents/MacOS/krill-app")).unwrap();
    assert!(matches!(
        validate_bundle_layout(&bundle),
        Err(Error::BundleStructure(_))
    ));
}

#[test]
fn current_app_bundle_prefers_explicit_path_and_derives_from_exe() {
    // 명시 경로(앞뒤 공백 무시)가 최우선이다.
    assert_eq!(
        current_app_bundle_in(Some("  /tmp/Krill.app\n"), Path::new("/x")),
        Some(PathBuf::from("/tmp/Krill.app"))
    );
    // 실행 파일 위치에서 번들을 찾는다.
    assert_eq!(
        current_app_bundle_in(
            None,
            Path::new("/Applications/Krill.app/Contents/MacOS/krill-app")
        ),
        Some(PathBuf::from("/Applications/Krill.app"))
    );
    // 번들 같지 않은 실행 파일이면 None.
    assert_eq!(
        current_app_bundle_in(None, Path::new("/usr/local/bin/krill")),
        None
    );
}

#[test]
fn poll_update_with_wires_manifest_url_without_relaunch() {
    let d = TempGuard(temp_dir("poll-env"));
    let state = d.0.join("state");
    let exe = std::env::current_exe().unwrap();
    let bundle = d.0.join("Krill.app").to_string_lossy().into_owned();
    // URL이 없거나 비어 있으면 점검을 건너뛴다.
    assert!(
        poll_update_with(None, Some(&bundle), false, &exe, &state)
            .unwrap()
            .is_none()
    );
    assert!(
        poll_update_with(Some("  \n"), Some(&bundle), false, &exe, &state)
            .unwrap()
            .is_none()
    );

    // updater.toml 공개키를 갖춰도 비루프백 URL은 거부된다(URL 값이 그대로 fetch로 간다).
    fs::create_dir_all(&state).unwrap();
    let (_kp, pk) = fresh_key();
    fs::write(
        state.join(CONFIG_FILE),
        format!("public_key = \"{}\"\n", encode_hex(&pk)),
    )
    .unwrap();
    assert!(matches!(
        poll_update_with(
            Some("http://evil.example.com/latest.json"),
            Some(&bundle),
            false,
            &exe,
            &state
        ),
        Err(Error::InsecureUrl(_))
    ));
}

#[test]
fn stage_update_manifest_downloads_and_stages_from_local_http() {
    let d = TempGuard(temp_dir("stage-env"));
    let state = d.0.join("state");
    fs::create_dir_all(&state).unwrap();
    let (kp, pk) = fresh_key();
    fs::write(
        state.join(CONFIG_FILE),
        format!("public_key = \"{}\"\n", encode_hex(&pk)),
    )
    .unwrap();

    let app = d.0.join("Krill.app");
    build_test_app_bundle(&app, "0.1.0", "exit 0", true);

    let v2_src = d.0.join("src-0.2.0/Krill.app");
    build_test_app_bundle(&v2_src, "0.2.0", "exit 0", true);
    let v2_zip = zip_bundle(&v2_src, &d.0.join("v2.zip"));

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base_url = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
    let mut m = Manifest {
        version: "0.2.0".to_owned(),
        url: format!("{base_url}/Krill-0.2.0.zip"),
        sha256: sha256_hex(&v2_zip),
        ed25519_sig: String::new(),
        min_daemon_protocol: 1,
        channel: Channel::Stable,
        rollout_pct: 100,
    };
    sign_manifest_with(&kp, &mut m);
    let m_json = serde_json::to_vec(&m).unwrap();
    let server = thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let mut stream = stream;
            let mut buf = [0u8; 4096];
            let n = stream.read(&mut buf).unwrap_or(0);
            let req = String::from_utf8_lossy(&buf[..n]);
            let path = req
                .lines()
                .next()
                .and_then(|l| l.split_whitespace().nth(1))
                .unwrap_or("/");
            let body: &[u8] = match path {
                "/latest.json" => &m_json,
                "/Krill-0.2.0.zip" => &v2_zip,
                "/stop" => break,
                _ => b"not found",
            };
            let header = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            stream.write_all(header.as_bytes()).ok();
            stream.write_all(body).ok();
        }
    });

    // 현재 버전(0.1.1)보다 새 매니페스트라 스테이징까지 간다. 원래 번들은 그대로다.
    let staged = stage_update_manifest(&format!("{base_url}/latest.json"), &app, &state).unwrap();
    assert_eq!(staged, Some((state.clone(), "0.2.0".to_owned())));
    assert_eq!(bundle_version(&staging_path(&app)), "0.2.0");
    assert_eq!(bundle_version(&app), "0.1.0");

    fetch_bytes(&format!("{base_url}/stop"), 64).ok();
    server.join().ok();
}

/// AS-05: 점진 롤아웃 버킷은 기기마다 안정적이고 0..100에 든다.
#[test]
fn rollout_bucket_is_stable_per_state_dir() {
    let a = Path::new("/tmp/mk-state-a");
    let b = Path::new("/tmp/mk-state-b");
    assert_eq!(rollout_bucket(a), rollout_bucket(a));
    assert!(rollout_bucket(a) < 100);
    assert!(rollout_bucket(b) < 100);
}

/// AS-05: `download_and_stage`는 호출자가 준 버킷으로 판정한다. 롤아웃
/// 대상 밖이면 네트워크에 손대지 않고 `Ok(None)`을 돌려준다.
#[test]
fn download_and_stage_honors_rollout_bucket_without_network() {
    let d = TempGuard(temp_dir("rollout-bucket"));
    let sk = decode_hex::<32>("9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60")
        .unwrap();
    let pk = test_public_key(&sk);
    let app = d.0.join("Krill.app");
    let mut m = Manifest {
        version: "0.2.0".to_owned(),
        // 도달 불가 루프백: 대상 밖 판정이면 fetch를 시도조차 하면 안 된다.
        url: "https://127.0.0.1:1/Krill-0.2.0.zip".to_owned(),
        sha256: sha256_hex(b"archive-bytes"),
        ed25519_sig: String::new(),
        min_daemon_protocol: 1,
        channel: Channel::Stable,
        rollout_pct: 10,
    };
    sign_manifest(&mut m, &sk);
    let staged = download_and_stage(
        &m,
        &pk,
        "0.1.0",
        Channel::Stable,
        &app,
        &CodesignVerifier::adhoc(),
        50,
    )
    .unwrap();
    assert!(
        staged.is_none(),
        "bucket 50 must be deferred by rollout_pct 10"
    );
    assert!(!staging_path(&app).exists());
}

/// AS-09: 교체가 끝난 뒤(`staging` 없음) 죽었으면 성공한 교체를 `RolledBack`으로
/// 오표기하면 안 된다. 검증 단계로 이어간다.
#[test]
fn recover_journal_completed_swap_continues_verification() {
    let d = TempGuard(temp_dir("journal-done"));
    let state = d.0.join("state");
    let app = d.0.join("Krill.app");
    let prev = prev_path(&app);
    build_test_app_bundle(&app, "0.2.0", "exit 0", true);
    build_test_app_bundle(&prev, "0.1.0", "exit 0", true);
    assert!(!staging_path(&app).exists());
    let j = Journal {
        bundle: app.clone(),
        from_version: "0.1.0".to_owned(),
        to_version: "0.2.0".to_owned(),
        health_timeout_ms: 5_000,
        stage: Stage::MovingStagingToTarget,
    };
    j.save(&state).unwrap();
    let recovered = recover_journal(&state).unwrap().unwrap();
    assert_eq!(recovered.stage, Stage::VerifyingLaunch);
    assert_eq!(bundle_version(&app), "0.2.0");
    assert_eq!(bundle_version(&prev), "0.1.0");
}

/// AS-09: 교체 전(`bundle`·`staging` 둘 다 멀쩡)에 죽었으면 복원할 게 없으니
/// 거짓 롤백 대화상자를 남기지 않고 저널을 버린다.
#[test]
fn recover_journal_before_swap_drops_stale_journal() {
    let d = TempGuard(temp_dir("journal-preswap"));
    let state = d.0.join("state");
    let app = d.0.join("Krill.app");
    let staging = staging_path(&app);
    build_test_app_bundle(&app, "0.1.0", "exit 0", true);
    build_test_app_bundle(&staging, "0.2.0", "exit 0", true);
    let j = Journal {
        bundle: app.clone(),
        from_version: "0.1.0".to_owned(),
        to_version: "0.2.0".to_owned(),
        health_timeout_ms: 5_000,
        stage: Stage::MovingToPrev,
    };
    j.save(&state).unwrap();
    assert!(recover_journal(&state).unwrap().is_none());
    assert!(!journal_path(&state).exists());
    assert_eq!(bundle_version(&app), "0.1.0");
    assert_eq!(bundle_version(&staging), "0.2.0");
}

/// APP-F02: `InPlace` 백업(`app -> .prev`)이 도중에 실패하면 불완전한 `.prev`를 복원 원천으로 삼아
/// 아직 백업되지 않은 원본 파일을 prune하면 안 된다. 번들은 그대로, `.prev`는 제거, 저널은 `RolledBack`.
#[test]
fn inplace_backup_failure_keeps_original_bundle_intact() {
    #[derive(Debug)]
    struct Accept;
    impl CodeVerifier for Accept {
        fn verify(&self, _: &Path) -> Result<(), Error> {
            Ok(())
        }
    }
    // UDS 경로 상한(104바이트) 때문에 짧은 `/tmp` 경로를 쓴다.
    let root = PathBuf::from("/tmp").join(format!("mk-updater-bf-{}", std::process::id()));
    fs::remove_dir_all(&root).ok();
    fs::create_dir_all(&root).unwrap();
    let d = TempGuard(root);
    let state = d.0.join("state");
    let app = d.0.join("Krill.app");
    let staging = staging_path(&app);
    let prev = prev_path(&app);
    build_test_app_bundle(&app, "0.1.0", "exit 0", false);
    build_test_app_bundle(&staging, "0.2.0", "exit 0", false);
    // 소켓 파일: `fs::copy`가 정규 파일이 아니라며 권한·read_dir 순서와 무관하게 실패한다(백업 중단 주입).
    // 이 항목은 `.prev`에 절대 복사되지 않으므로 옛 코드에서는 prune으로 지워졌다.
    let socket = app.join("Contents/Resources/l.sock");
    let _listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();

    let result = relaunch::swap_with(
        relaunch::Mode::InPlace,
        &app,
        &state,
        "0.1.0",
        "0.2.0",
        Duration::from_secs(1),
        &Accept,
    );
    assert!(result.is_err(), "{result:?}");

    assert!(!prev.exists(), "partial backup must not survive");
    assert_eq!(bundle_version(&app), "0.1.0");
    assert!(socket.symlink_metadata().is_ok(), "original entry pruned");
    assert!(app.join("Contents/MacOS/krill-app").is_file());
    assert!(app.join("Contents/Info.plist").is_file());
    assert_eq!(bundle_version(&staging), "0.2.0");
    let journal = Journal::load(&state).unwrap().unwrap();
    assert!(
        matches!(journal.stage, Stage::RolledBack { .. }),
        "{journal:?}"
    );
}

/// APP-F02: `rollback_with`를 백업 단계(`MovingToPrev`) 저널과 빈 `.prev`로 직접 불러도
/// 번들에서 아무 파일도 지우지 않는다.
#[test]
fn inplace_rollback_during_backup_stage_never_prunes_original() {
    let d = TempGuard(temp_dir("inplace-partial-rollback"));
    let state = d.0.join("state");
    let app = d.0.join("Krill.app");
    let prev = prev_path(&app);
    build_test_app_bundle(&app, "0.1.0", "exit 0", false);
    fs::create_dir_all(prev.join("Contents")).unwrap();
    Journal {
        bundle: app.clone(),
        from_version: "0.1.0".to_owned(),
        to_version: "0.2.0".to_owned(),
        health_timeout_ms: 5_000,
        stage: Stage::MovingToPrev,
    }
    .save(&state)
    .unwrap();

    relaunch::rollback_with(relaunch::Mode::InPlace, &app, &state, "backup failed").unwrap();

    assert!(!prev.exists());
    assert_eq!(bundle_version(&app), "0.1.0");
    assert!(app.join("Contents/MacOS/krill-app").is_file());
    assert!(app.join("Contents/Info.plist").is_file());
    assert_eq!(
        Journal::load(&state).unwrap().unwrap().stage,
        Stage::RolledBack {
            reason: "backup failed".to_owned()
        }
    );
}

/// APP-F03: 파일 단위 교체(`MovingStagingToTarget`) 도중 죽어 번들에 신·구 파일이 섞이고 스테이징이
/// 남아 있으면, 완결된 `.prev`로 되돌리고 `RolledBack`을 기록한다. 저널을 버려 혼합 번들을 정상
/// 설치로 오판하면 안 된다. 복구는 멱등이다.
#[test]
fn inplace_recovery_restores_backup_after_interrupted_replacement() {
    let d = TempGuard(temp_dir("inplace-mixed"));
    let state = d.0.join("state");
    let app = d.0.join("Krill.app");
    let prev = prev_path(&app);
    let staging = staging_path(&app);
    build_test_app_bundle(&prev, "0.1.0", "exit 0", false);
    fs::write(prev.join("Contents/Resources/old-only.txt"), "old").unwrap();
    build_test_app_bundle(&staging, "0.2.0", "exit 0", false);
    fs::write(staging.join("Contents/Resources/new-only.txt"), "new").unwrap();
    // 혼합 번들: 메타데이터·new-only는 새 버전으로 바뀌었고 old-only는 아직 지워지지 않았다.
    build_test_app_bundle(&app, "0.2.0", "exit 0", false);
    fs::write(app.join("Contents/Resources/new-only.txt"), "new").unwrap();
    fs::write(app.join("Contents/Resources/old-only.txt"), "old").unwrap();
    Journal {
        bundle: app.clone(),
        from_version: "0.1.0".to_owned(),
        to_version: "0.2.0".to_owned(),
        health_timeout_ms: 5_000,
        stage: Stage::MovingStagingToTarget,
    }
    .save(&state)
    .unwrap();

    let recovered = relaunch::recover_with(relaunch::Mode::InPlace, &state)
        .unwrap()
        .unwrap();
    let rolled_back = Stage::RolledBack {
        reason: "interrupted during bundle replacement".to_owned(),
    };
    assert_eq!(recovered.stage, rolled_back);
    assert_eq!(bundle_version(&app), "0.1.0");
    assert!(!app.join("Contents/Resources/new-only.txt").exists());
    assert_eq!(
        fs::read_to_string(app.join("Contents/Resources/old-only.txt")).unwrap(),
        "old"
    );
    assert!(!prev.exists());
    assert_eq!(Journal::load(&state).unwrap().unwrap(), recovered);

    let again = relaunch::recover_with(relaunch::Mode::InPlace, &state)
        .unwrap()
        .unwrap();
    assert_eq!(again, recovered);
    assert_eq!(bundle_version(&app), "0.1.0");
}

/// 저널을 읽을 수 없으면 백업 상태를 추정하지 않는다: `rollback_with`는 오류를 돌려주고 번들·`.prev`를
/// 건드리지 않는다(불완전한 `.prev`를 완결로 오판해 번들을 prune하는 일이 없다).
#[test]
fn inplace_rollback_refuses_unreadable_journal() {
    let d = TempGuard(temp_dir("inplace-corrupt-journal"));
    let state = d.0.join("state");
    let app = d.0.join("Krill.app");
    let prev = prev_path(&app);
    build_test_app_bundle(&app, "0.1.0", "exit 0", false);
    fs::create_dir_all(prev.join("Contents")).unwrap();
    fs::create_dir_all(state.join(UPDATE_DIR)).unwrap();
    fs::write(journal_path(&state), b"{not json").unwrap();

    let err = relaunch::rollback_with(relaunch::Mode::InPlace, &app, &state, "x").unwrap_err();
    assert!(matches!(err, Error::Json(_)), "{err:?}");
    assert!(prev.exists());
    assert_eq!(bundle_version(&app), "0.1.0");
    assert!(app.join("Contents/MacOS/krill-app").is_file());
    assert!(app.join("Contents/Info.plist").is_file());
}

/// 교체 단계에서 스테이징은 남았는데 백업이 없으면 복원할 수 없다. 복원했다고 기록하지 않고
/// `NoBackup`으로 알리며 저널·스테이징·번들을 그대로 둔다.
#[test]
fn inplace_recovery_without_backup_reports_no_backup() {
    let d = TempGuard(temp_dir("inplace-no-backup"));
    let state = d.0.join("state");
    let app = d.0.join("Krill.app");
    let staging = staging_path(&app);
    build_test_app_bundle(&app, "0.2.0", "exit 0", false);
    fs::write(app.join("Contents/Resources/old-only.txt"), "old").unwrap();
    build_test_app_bundle(&staging, "0.2.0", "exit 0", false);
    let journal = Journal {
        bundle: app.clone(),
        from_version: "0.1.0".to_owned(),
        to_version: "0.2.0".to_owned(),
        health_timeout_ms: 5_000,
        stage: Stage::MovingStagingToTarget,
    };
    journal.save(&state).unwrap();

    let err = relaunch::recover_with(relaunch::Mode::InPlace, &state).unwrap_err();
    assert!(matches!(err, Error::NoBackup(_)), "{err:?}");
    assert_eq!(Journal::load(&state).unwrap().unwrap(), journal);
    assert_eq!(bundle_version(&staging), "0.2.0");
    assert_eq!(bundle_version(&app), "0.2.0");
    assert!(app.join("Contents/Resources/old-only.txt").is_file());
}

/// APP-F02/F03: 백업 단계(`MovingToPrev`)에서 죽어 불완전한 `.prev`가 남아 있으면 번들은 그대로 두고
/// `.prev`와 저널을 버린다(불완전 백업이 다음 롤백의 원천이 되지 않는다).
#[test]
fn inplace_recovery_before_backup_completes_drops_partial_prev() {
    let d = TempGuard(temp_dir("inplace-partial-prev"));
    let state = d.0.join("state");
    let app = d.0.join("Krill.app");
    let prev = prev_path(&app);
    let staging = staging_path(&app);
    build_test_app_bundle(&app, "0.1.0", "exit 0", false);
    build_test_app_bundle(&staging, "0.2.0", "exit 0", false);
    fs::create_dir_all(prev.join("Contents/Resources")).unwrap();
    fs::write(prev.join("Contents/Resources/version.txt"), "0.1.0").unwrap();
    Journal {
        bundle: app.clone(),
        from_version: "0.1.0".to_owned(),
        to_version: "0.2.0".to_owned(),
        health_timeout_ms: 5_000,
        stage: Stage::MovingToPrev,
    }
    .save(&state)
    .unwrap();

    assert!(
        relaunch::recover_with(relaunch::Mode::InPlace, &state)
            .unwrap()
            .is_none()
    );
    assert!(!journal_path(&state).exists());
    assert!(!prev.exists());
    assert_eq!(bundle_version(&app), "0.1.0");
    assert!(app.join("Contents/MacOS/krill-app").is_file());
    assert_eq!(bundle_version(&staging), "0.2.0");
}

#[test]
fn as12_manifest_rejects_unsigned_future_policy_fields() {
    let mut value = serde_json::json!({
        "version": "0.2.0", "url": "https://github.com/craftowen/krill/releases/download/v0.2.0/app.zip",
        "sha256": "00".repeat(32), "ed25519_sig": "00".repeat(64),
        "min_daemon_protocol": 1, "channel": "stable", "rollout_pct": 100
    });
    value["minimum_app_version"] = serde_json::json!("999.0.0");
    assert!(Manifest::parse(&value.to_string()).is_err());
}

#[test]
fn as12_update_journal_rejects_unknown_recovery_policy() {
    let raw = r#"{"bundle":"/tmp/fake.app","from_version":"0.1.0","to_version":"0.2.0","health_timeout_ms":5000,"stage":{"state":"healthy"},"future_recovery":"required"}"#;
    assert!(serde_json::from_str::<Journal>(raw).is_err());
}

#[test]
fn ed25519_manifest_py_cross_verifies_with_rust_manifest() {
    let d = TempGuard(temp_dir("ed25519-py-cross"));
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    let script = repo_root.join("packaging/macos/ed25519_manifest.py");
    let key_file = d.0.join("release.ed25519.key");
    let out_manifest = d.0.join("latest.json");

    let keygen = Command::new("/usr/bin/python3")
        .arg(&script)
        .args(["keygen", "--key-file"])
        .arg(&key_file)
        .output()
        .unwrap();
    assert!(
        keygen.status.success(),
        "{}",
        String::from_utf8_lossy(&keygen.stderr)
    );
    let pub_hex = String::from_utf8(keygen.stdout).unwrap().trim().to_owned();
    let pub_bytes = decode_hex::<32>(&pub_hex).unwrap();

    let sha = sha256_hex(b"sample-zip-payload");
    let sign = Command::new("/usr/bin/python3")
        .arg(&script)
        .args([
            "sign",
            "--key-file",
            &key_file.to_string_lossy(),
            "--version",
            "0.2.0",
            "--url",
            "https://github.com/craftowen/krill/releases/download/v0.2.0/Krill-0.2.0-arm64-adhoc.zip",
            "--sha256",
            &sha,
            "--min-daemon-protocol",
            "1",
            "--channel",
            "stable",
            "--rollout-pct",
            "100",
            "--output",
            &out_manifest.to_string_lossy(),
        ])
        .output()
        .unwrap();
    assert!(
        sign.status.success(),
        "{}",
        String::from_utf8_lossy(&sign.stderr)
    );

    let json_text = fs::read_to_string(&out_manifest).unwrap();
    let manifest = Manifest::parse(&json_text).unwrap();
    manifest.verify_signature(&pub_bytes).unwrap();
    assert_eq!(
        manifest
            .evaluate(&pub_bytes, "0.1.1", Channel::Stable, 1, 0)
            .unwrap(),
        Decision::Available
    );
}

#[test]
fn info_plist_zero_config_stages_and_loads_without_updater_toml() {
    let d = TempGuard(temp_dir("plist-zero-cfg"));
    let state = d.0.join("state");
    fs::create_dir_all(&state).unwrap();
    let (kp, pk) = fresh_key();
    let pk_hex = encode_hex(&pk);

    let v2_src = d.0.join("src-0.2.0/Krill.app");
    build_test_app_bundle(
        &v2_src,
        "0.2.0",
        "printf 'ok\\n' > \"$KRILL_UPDATE_READY_FILE\"\nsleep 0.2\nexit 0",
        true,
    );
    let v2_zip = zip_bundle(&v2_src, &d.0.join("v2.zip"));

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base_url = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
    let manifest_url = format!("{base_url}/latest.json");

    let app = d.0.join("Krill.app");
    build_test_app_bundle(&app, "0.1.0", "exit 0", false);
    for (k, v) in [
        ("KrillUpdatePublicKey", pk_hex.as_str()),
        ("KrillUpdateURL", manifest_url.as_str()),
    ] {
        let st = Command::new("/usr/bin/plutil")
            .args(["-insert", k, "-string", v])
            .arg(app.join("Contents/Info.plist"))
            .status()
            .unwrap();
        assert!(st.success());
    }
    let cs = Command::new("/usr/bin/codesign")
        .args(["--force", "-s", "-"])
        .arg(&app)
        .status()
        .unwrap();
    assert!(cs.success());

    // updater.toml이 없어도 Info.plist에서 공개키·URL·버전을 읽는다.
    let cfg = Config::load_for_bundle(&state, Some(&app)).unwrap();
    assert_eq!(cfg.public_key, pk_hex);
    assert_eq!(cfg.url, manifest_url);
    assert_eq!(bundle_short_version(&app).as_deref(), Some("0.1.0"));

    let mut m = Manifest {
        version: "0.2.0".to_owned(),
        url: format!("{base_url}/Krill-0.2.0.zip"),
        sha256: sha256_hex(&v2_zip),
        ed25519_sig: String::new(),
        min_daemon_protocol: 1,
        channel: Channel::Stable,
        rollout_pct: 100,
    };
    sign_manifest_with(&kp, &mut m);
    let m_json = serde_json::to_vec(&m).unwrap();
    let server = thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let mut stream = stream;
            let mut buf = [0u8; 4096];
            let n = stream.read(&mut buf).unwrap_or(0);
            let req = String::from_utf8_lossy(&buf[..n]);
            let path = req
                .lines()
                .next()
                .and_then(|l| l.split_whitespace().nth(1))
                .unwrap_or("/");
            let body: &[u8] = match path {
                "/latest.json" => &m_json,
                "/Krill-0.2.0.zip" => &v2_zip,
                "/stop" => break,
                _ => b"not found",
            };
            let header = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            stream.write_all(header.as_bytes()).ok();
            stream.write_all(body).ok();
        }
    });

    let exe = std::env::current_exe().unwrap();
    let app_str = app.to_string_lossy().into_owned();
    let staged = poll_update_with(None, Some(&app_str), false, &exe, &state)
        .unwrap()
        .expect("must stage from Info.plist without manifest_url or updater.toml");
    assert_eq!(staged.from_version, "0.1.0");
    assert_eq!(staged.to_version, "0.2.0");
    assert!(!staged.relaunched);
    assert_eq!(bundle_version(&staging_path(&app)), "0.2.0");

    // updater.toml이 없는 상태 디렉터리에서 분리 relauncher 진입점(run_relauncher_args)도
    // 번들의 Info.plist로 검증·교체·기동 확인을 완료한다(MK-155 F3).
    assert!(!state.join(CONFIG_FILE).exists());
    relaunch::run_relauncher_args(
        [
            app.to_string_lossy().as_ref(),
            state.to_string_lossy().as_ref(),
            "0.1.0",
            "0.2.0",
            "5000",
        ]
        .into_iter()
        .map(std::ffi::OsString::from),
    )
    .expect("run_relauncher_args must succeed without updater.toml using Info.plist fallback");
    assert_eq!(bundle_version(&app), "0.2.0");
    assert_eq!(
        Journal::load(&state).unwrap().expect("journal").stage,
        Stage::Healthy
    );

    fetch_bytes(&format!("{base_url}/stop"), 64).ok();
    server.join().ok();
}

#[test]
fn github_release_asset_url_parsing_and_gh_fallback() {
    assert_eq!(
        parse_github_release_asset(
            "https://github.com/craftowen/krill/releases/latest/download/latest.json"
        ),
        Some(("craftowen/krill".to_owned(), None, "latest.json".to_owned()))
    );
    assert_eq!(
        parse_github_release_asset(
            "https://github.com/craftowen/krill/releases/download/v0.2.0/Krill-0.2.0-arm64-adhoc.zip"
        ),
        Some((
            "craftowen/krill".to_owned(),
            Some("v0.2.0".to_owned()),
            "Krill-0.2.0-arm64-adhoc.zip".to_owned()
        ))
    );
    assert!(parse_github_release_asset("https://example.com/latest.json").is_none());
    assert!(
        parse_github_release_asset(
            "https://github.com/craftowen/krill/releases/download/--evil/latest.json"
        )
        .is_none()
    );

    let d = TempGuard(temp_dir("gh-fallback"));
    let fake_gh = d.0.join("fake-gh");
    fs::write(
        &fake_gh,
        "#!/bin/sh\nif [ \"$1\" = \"release\" ] && [ \"$2\" = \"download\" ]; then\n  printf 'gh-fallback-ok'\n  exit 0\nfi\nexit 1\n",
    )
    .unwrap();
    fs::set_permissions(&fake_gh, fs::Permissions::from_mode(0o755)).unwrap();

    let got = fetch_github_release_asset_with_bin(
        &fake_gh,
        "craftowen/krill",
        Some("v0.2.0"),
        "latest.json",
        1024,
    )
    .unwrap();
    assert_eq!(got, b"gh-fallback-ok");
}

#[test]
fn update_cli_check_and_headless_apply_updates_bundle_and_marks_healthy() {
    let d = TempGuard(temp_dir("update-cli"));
    let state = d.0.join("state");
    fs::create_dir_all(&state).unwrap();
    let (kp, pk) = fresh_key();
    let pk_hex = encode_hex(&pk);

    let v2_src = d.0.join("src-0.2.0/Krill.app");
    build_test_app_bundle(&v2_src, "0.2.0", "exit 0", true);
    let v2_zip = zip_bundle(&v2_src, &d.0.join("v2.zip"));

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base_url = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
    let manifest_url = format!("{base_url}/latest.json");

    let app = d.0.join("Krill.app");
    build_test_app_bundle(&app, "0.1.0", "exit 0", false);
    for (k, v) in [
        ("KrillUpdatePublicKey", pk_hex.as_str()),
        ("KrillUpdateURL", manifest_url.as_str()),
    ] {
        let st = Command::new("/usr/bin/plutil")
            .args(["-insert", k, "-string", v])
            .arg(app.join("Contents/Info.plist"))
            .status()
            .unwrap();
        assert!(st.success());
    }
    assert!(
        Command::new("/usr/bin/codesign")
            .args(["--force", "-s", "-"])
            .arg(&app)
            .status()
            .unwrap()
            .success()
    );

    let mut m = Manifest {
        version: "0.2.0".to_owned(),
        url: format!("{base_url}/Krill-0.2.0.zip"),
        sha256: sha256_hex(&v2_zip),
        ed25519_sig: String::new(),
        min_daemon_protocol: 1,
        channel: Channel::Stable,
        rollout_pct: 100,
    };
    sign_manifest_with(&kp, &mut m);
    let m_json = serde_json::to_vec(&m).unwrap();
    let server = thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let mut stream = stream;
            let mut buf = [0u8; 4096];
            let n = stream.read(&mut buf).unwrap_or(0);
            let req = String::from_utf8_lossy(&buf[..n]);
            let path = req
                .lines()
                .next()
                .and_then(|l| l.split_whitespace().nth(1))
                .unwrap_or("/");
            let body: &[u8] = match path {
                "/latest.json" => &m_json,
                "/Krill-0.2.0.zip" => &v2_zip,
                "/stop" => break,
                _ => b"not found",
            };
            let header = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            stream.write_all(header.as_bytes()).ok();
            stream.write_all(body).ok();
        }
    });

    let check_code = run_update_cli_args(
        [
            "--check",
            "--json",
            "--app",
            &app.to_string_lossy(),
            "--state-dir",
            &state.to_string_lossy(),
        ]
        .into_iter()
        .map(std::ffi::OsString::from),
    )
    .unwrap();
    assert_eq!(check_code, 0);
    assert_eq!(bundle_version(&app), "0.1.0");

    let apply_code = run_update_cli_args(
        [
            "--json",
            "--app",
            &app.to_string_lossy(),
            "--state-dir",
            &state.to_string_lossy(),
        ]
        .into_iter()
        .map(std::ffi::OsString::from),
    )
    .unwrap();
    assert_eq!(apply_code, 0);
    assert_eq!(bundle_version(&app), "0.2.0");
    let journal = Journal::load(&state).unwrap().expect("journal saved");
    assert_eq!(journal.stage, Stage::Healthy);

    fetch_bytes(&format!("{base_url}/stop"), 64).ok();
    server.join().ok();
}

/// OWE-883: 롤백 사유 영문을 사용자용 문구로 바꾼다. 모르는 사유는 그대로 둔다.
#[test]
fn owe883_localize_reason_maps_known_english_reasons() {
    assert_eq!(
        relaunch::localize_reason("interrupted during bundle replacement"),
        "교체 중 중단됨"
    );
    assert_eq!(
        relaunch::localize_reason("old app did not exit; replacement aborted"),
        "이전 앱이 종료되지 않아 교체 중단됨"
    );
    let exited = relaunch::localize_reason("new app exited before ready (exit status: 1)");
    assert!(exited.contains("exit status: 1"), "{exited}");
    assert!(!exited.contains("new app exited before ready"), "{exited}");
    let timeout = relaunch::localize_reason("new app did not report ready within 30000ms");
    assert!(timeout.contains("30000"), "{timeout}");
    assert!(!timeout.contains("did not report ready"), "{timeout}");
    assert_eq!(relaunch::localize_reason("mystery"), "mystery");
}

/// OWE-837: Windows 이름 바꾸기 교체([`relaunch::Mode::Rename`])를 플랫폼 중립 파일 연산으로 검증한다.
/// 실행 중 exe를 지울 수 없는 Windows 특성은 VM에서 실측했다(docs/research/windows-spike-OWE-837.md).
mod rename_mode {
    use super::super::relaunch::{Mode, recover_with, rollback_with, swap_with};
    use super::*;

    #[derive(Debug)]
    struct Accept;
    impl CodeVerifier for Accept {
        fn verify(&self, _: &Path) -> Result<(), Error> {
            Ok(())
        }
    }

    fn write(dir: &Path, rel: &str, body: &str) {
        let path = dir.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, body).unwrap();
    }

    fn read(dir: &Path, rel: &str) -> Option<String> {
        fs::read_to_string(dir.join(rel)).ok()
    }

    /// 구 설치(krill-link.exe는 새 패키지에서 빠짐)와 새 스테이징(conpty.dll 추가).
    fn layout(root: &Path) -> (PathBuf, PathBuf) {
        let install = root.join("Krill");
        let staging = staging_path(&install);
        for rel in [
            "Krill.json",
            "krill-app.exe",
            "krilld.exe",
            "x64/OpenConsole.exe",
            "krill-link.exe",
        ] {
            write(&install, rel, "old");
        }
        for rel in [
            "Krill.json",
            "krill-app.exe",
            "krilld.exe",
            "x64/OpenConsole.exe",
            "conpty.dll",
        ] {
            write(&staging, rel, "new");
        }
        write(
            &staging,
            BUNDLE_INFO,
            r#"<?xml version="1.0"?><plist version="1.0"><dict><key>CFBundleShortVersionString</key><string>0.2.0</string></dict></plist>"#,
        );
        (install, staging)
    }

    fn swap(install: &Path, state: &Path) -> Result<Journal, Error> {
        swap_with(
            Mode::Rename,
            install,
            state,
            "0.1.0",
            "0.2.0",
            Duration::from_secs(1),
            &Accept,
        )
    }

    #[test]
    fn updater_rename_swap_then_rollback_restores_old_files() {
        let d = TempGuard(temp_dir("rename-swap"));
        let state = d.0.join("state");
        let (install, staging) = layout(&d.0);

        let journal = swap(&install, &state).unwrap();
        assert_eq!(journal.stage, Stage::VerifyingLaunch);
        assert!(!staging.exists());
        assert_eq!(
            read(&install, "x64/OpenConsole.exe").as_deref(),
            Some("new")
        );
        assert_eq!(read(&install, "conpty.dll").as_deref(), Some("new"));
        assert_eq!(read(&install, "krill-link.exe"), None);
        assert_eq!(
            read(&prev_path(&install), "krill-app.exe").as_deref(),
            Some("old")
        );
        assert_eq!(
            read(&prev_path(&install), "krill-link.exe").as_deref(),
            Some("old")
        );

        rollback_with(Mode::Rename, &install, &state, "health").unwrap();
        for rel in [
            "Krill.json",
            "krill-app.exe",
            "krilld.exe",
            "x64/OpenConsole.exe",
            "krill-link.exe",
        ] {
            assert_eq!(read(&install, rel).as_deref(), Some("old"), "{rel}");
        }
        assert_eq!(
            read(&install, "conpty.dll"),
            None,
            "new-only file must not survive rollback"
        );
        assert!(!prev_path(&install).exists());
        let j = Journal::load(&state).unwrap().unwrap();
        assert!(matches!(j.stage, Stage::RolledBack { .. }), "{j:?}");
    }

    #[test]
    fn updater_rename_refuses_unknown_files_in_install_dir() {
        let d = TempGuard(temp_dir("rename-unknown"));
        let state = d.0.join("state");
        let (install, staging) = layout(&d.0);
        write(&install, "notes.txt", "user data");

        let err = swap(&install, &state).unwrap_err();
        assert!(err.to_string().contains("notes.txt"), "{err}");
        assert_eq!(read(&install, "notes.txt").as_deref(), Some("user data"));
        assert_eq!(read(&install, "krill-app.exe").as_deref(), Some("old"));
        assert!(staging.exists());
        assert!(Journal::load(&state).unwrap().is_none());
    }

    #[test]
    fn updater_rename_failure_mid_move_restores_without_losing_old_files() {
        let d = TempGuard(temp_dir("rename-midfail"));
        let state = d.0.join("state");
        let (install, _staging) = layout(&d.0);
        // 쓰기 금지 폴더에서는 파일을 옮길 수 없다 → 1단계(구 파일 옮기기) 중 실패를 만든다.
        let x64 = install.join("x64");
        fs::set_permissions(&x64, fs::Permissions::from_mode(0o500)).unwrap();
        let err = swap(&install, &state);
        fs::set_permissions(&x64, fs::Permissions::from_mode(0o700)).unwrap();
        assert!(err.is_err(), "{err:?}");
        for rel in [
            "Krill.json",
            "krill-app.exe",
            "krilld.exe",
            "x64/OpenConsole.exe",
            "krill-link.exe",
        ] {
            assert_eq!(read(&install, rel).as_deref(), Some("old"), "{rel}");
        }
        let j = Journal::load(&state).unwrap().unwrap();
        assert!(matches!(j.stage, Stage::RolledBack { .. }), "{j:?}");
    }

    #[test]
    fn updater_rename_recover_interrupted_moves() {
        // 1단계(구 파일 옮기기) 중 중단: 옮긴 파일을 되돌리고 저널을 버린다.
        let d = TempGuard(temp_dir("rename-recover"));
        let state = d.0.join("state");
        let (install, staging) = layout(&d.0);
        let prev = prev_path(&install);
        write(&prev, "krill-app.exe", "old");
        fs::remove_file(install.join("krill-app.exe")).unwrap();
        let mut j = Journal {
            bundle: install.clone(),
            from_version: "0.1.0".to_owned(),
            to_version: "0.2.0".to_owned(),
            health_timeout_ms: 1_000,
            stage: Stage::MovingToPrev,
        };
        j.save(&state).unwrap();
        assert!(recover_with(Mode::Rename, &state).unwrap().is_none());
        assert_eq!(read(&install, "krill-app.exe").as_deref(), Some("old"));
        assert!(!journal_path(&state).exists());

        // 2단계(새 파일 놓기) 중 중단: 남은 스테이징을 마저 옮겨 검증 단계로 이어간다.
        rename::move_tree(&install, &prev, &trash_path(&install)).unwrap();
        fs::rename(staging.join("krill-app.exe"), install.join("krill-app.exe")).unwrap();
        j.stage = Stage::MovingStagingToTarget;
        j.save(&state).unwrap();
        let recovered = recover_with(Mode::Rename, &state).unwrap().unwrap();
        assert_eq!(recovered.stage, Stage::VerifyingLaunch);
        assert!(!staging.exists());
        for rel in [
            "Krill.json",
            "krill-app.exe",
            "krilld.exe",
            "x64/OpenConsole.exe",
            "conpty.dll",
        ] {
            assert_eq!(read(&install, rel).as_deref(), Some("new"), "{rel}");
        }
    }
}

#[test]
fn archive_file_hash_streams_and_checks_size_before_publish() {
    let dir = TempGuard(temp_dir("stream-hash"));
    let path = dir.0.join("archive");
    let bytes = vec![17_u8; 3 * 65536 + 7];
    fs::write(&path, &bytes).unwrap();
    assert_eq!(archive_sha(&path).unwrap(), sha256_hex(&bytes));
    OpenOptions::new()
        .write(true)
        .open(&path)
        .unwrap()
        .set_len(u64::try_from(MAX_ARCHIVE_BYTES).unwrap() + 1)
        .unwrap();
    assert!(archive_sha(&path).is_err());
}

#[test]
fn streamed_download_creates_private_file_without_archive_vec() {
    let dir = TempGuard(temp_dir("stream-download"));
    let path = dir.0.join("download.zip");
    stream_download(
        Command::new("/bin/sh").args(["-c", "printf 'archive-bytes'"]),
        &path,
    )
    .unwrap();
    assert_eq!(fs::read(&path).unwrap(), b"archive-bytes");
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    // create_new refuses to follow/overwrite an earlier destination.
    assert!(
        stream_download(
            Command::new("/bin/sh").args(["-c", "printf overwrite"]),
            &path
        )
        .is_err()
    );
    assert_eq!(fs::read(&path).unwrap(), b"archive-bytes");
}

#[test]
fn update_single_flight_reuses_inflight_success_and_releases_after_failure() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let directory = TempGuard(temp_dir("flight"));
    let key = directory.0.display().to_string();
    let calls = Arc::new(AtomicUsize::new(0));
    let (started, ready) = std::sync::mpsc::channel();
    let (release, held) = std::sync::mpsc::channel();
    let owner_key = key.clone();
    let owner_calls = calls.clone();
    let owner = thread::spawn(move || {
        single_flight(&owner_key, || {
            owner_calls.fetch_add(1, Ordering::Relaxed);
            started.send(()).unwrap();
            held.recv().unwrap();
            Ok(CheckStatus::UpToDate {
                version: "1.0.0".into(),
            })
        })
    });
    ready.recv().unwrap();
    let waiting_calls = calls.clone();
    let (entered, at_flight) = std::sync::mpsc::channel();
    let waiting_key = key.clone();
    let waiter = thread::spawn(move || {
        entered.send(()).unwrap();
        single_flight(&waiting_key, || {
            waiting_calls.fetch_add(1, Ordering::Relaxed);
            Ok(CheckStatus::UpToDate {
                version: "1.0.0".into(),
            })
        })
    });
    at_flight.recv().unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    while flight_registry()
        .lock()
        .unwrap()
        .get(&key)
        .unwrap()
        .state
        .lock()
        .unwrap()
        .waiters
        == 0
    {
        assert!(
            Instant::now() < deadline,
            "waiter did not reach the shared flight"
        );
        thread::yield_now();
    }
    release.send(()).unwrap();
    assert_eq!(
        owner.join().unwrap().unwrap(),
        waiter.join().unwrap().unwrap()
    );
    assert_eq!(calls.load(Ordering::Relaxed), 1);
    assert!(single_flight(&key, || Err(Error::BadSignature)).is_err());
    assert!(
        single_flight(&key, || Ok(CheckStatus::UpToDate {
            version: "1.0.0".into()
        }))
        .is_ok()
    );
}

#[test]
fn completed_flight_waiter_keeps_its_result_when_next_owner_starts() {
    let directory = TempGuard(temp_dir("flight-generations"));
    let key = directory.0.display().to_string();
    let (first, owner) = update_flight(&key);
    assert!(owner);
    let (waiting, owner) = update_flight(&key);
    assert!(!owner);
    {
        let mut state = first.state.lock().unwrap();
        state.result = Some(Ok(CheckStatus::UpToDate {
            version: "first".into(),
        }));
        state.running = false;
    }
    let (next, owner) = update_flight(&key);
    assert!(owner);
    assert!(!Arc::ptr_eq(&next, &waiting));
    assert_eq!(
        await_flight(&waiting).unwrap(),
        CheckStatus::UpToDate {
            version: "first".into()
        }
    );
    {
        let mut state = next.state.lock().unwrap();
        state.running = false;
        state.result = Some(Err(Error::BadSignature));
    }
    assert!(matches!(await_flight(&next), Err(Error::BadSignature)));
    assert_eq!(
        await_flight(&waiting).unwrap(),
        CheckStatus::UpToDate {
            version: "first".into()
        }
    );
}

#[test]
fn interrupted_stage_publication_restores_previous_or_finishes_verified_incoming() {
    for phase in ["prepared", "moved-old", "published", "initial"] {
        let dir = TempGuard(temp_dir(&format!("stage-publish-{phase}")));
        let bundle = dir.0.join("Krill.app");
        fs::create_dir_all(&bundle).unwrap();
        fs::write(bundle.join("installed"), "untouched").unwrap();
        let staging = staging_path(&bundle);
        let backup = append_ext(&bundle, "staging.previous");
        let incoming = append_ext(&bundle, "staging.incoming");
        if phase != "initial" {
            fs::create_dir_all(&staging).unwrap();
            fs::write(staging.join("version"), "old").unwrap();
        }
        fs::create_dir_all(&incoming).unwrap();
        fs::write(incoming.join("version"), "new").unwrap();
        write_stage_marker(&bundle).unwrap();
        if matches!(phase, "moved-old" | "published") {
            fs::rename(&staging, &backup).unwrap();
        }
        if phase == "published" {
            fs::rename(&incoming, &staging).unwrap();
        }
        recover_stage_publish_locked(&bundle).unwrap();
        assert_eq!(
            fs::read_to_string(staging.join("version")).unwrap(),
            if matches!(phase, "published" | "initial") {
                "new"
            } else {
                "old"
            }
        );
        assert_eq!(
            fs::read_to_string(bundle.join("installed")).unwrap(),
            "untouched"
        );
        assert!(!append_ext(&bundle, "stage-publish").exists());
        assert!(!backup.exists());
        assert!(!incoming.exists());
    }
}

#[test]
fn restart_swap_rejects_changed_staging_version_before_mutation() {
    #[derive(Debug)]
    struct Accept;
    impl CodeVerifier for Accept {
        fn verify(&self, _: &Path) -> Result<(), Error> {
            Ok(())
        }
    }
    let root = TempGuard(temp_dir("restart-stale-stage"));
    let app = root.0.join("Krill.app");
    let stage = staging_path(&app);
    build_test_app_bundle(&app, "0.1.0", "exit 0", false);
    build_test_app_bundle(&stage, "0.3.0", "exit 0", false);
    let state = root.0.join("state");
    assert!(
        swap_bundles(
            &app,
            &state,
            "0.1.0",
            "0.2.0",
            Duration::from_secs(1),
            &Accept
        )
        .is_err()
    );
    assert_eq!(bundle_short_version(&app).as_deref(), Some("0.1.0"));
    assert_eq!(bundle_short_version(&stage).as_deref(), Some("0.3.0"));
    assert!(!prev_path(&app).exists());
    assert!(!journal_path(&state).exists());
    fs::remove_file(stage.join(BUNDLE_INFO)).unwrap();
    assert!(
        swap_bundles(
            &app,
            &state,
            "0.1.0",
            "0.2.0",
            Duration::from_secs(1),
            &Accept
        )
        .is_err()
    );
    assert_eq!(bundle_short_version(&app).as_deref(), Some("0.1.0"));
    assert!(!prev_path(&app).exists());
}
