//! Check helper preparation without opening a desktop window or creating PTYs.
use super::*;

fn require_release_helpers() {
    assert_eq!(
        Path::new(APP).parent().unwrap().file_name().unwrap(),
        "release",
        "requires the release helper binaries"
    );
}

#[test]
#[ignore = "real release helpers; no GUI required"]
fn release_helpers_match_source_without_gui() {
    require_release_helpers();
    let dir = PathBuf::from(format!("/tmp/mk031-preflight-{}", std::process::id()));
    let (paths, guard, instance) = start_daemon(dir);
    assert!(guard.apps.is_empty());
    assert_eq!(guard.krilld, Some(u32::try_from(instance).unwrap()));
    assert!(matches!(request(&paths, &Request::List), Reply::Sessions(list) if list.is_empty()));
}

#[test]
#[ignore = "real release helpers; expected preparation failure, no GUI required"]
fn release_helper_mismatch_fails_before_gui_and_cleans_up() {
    require_release_helpers();
    let dir = PathBuf::from(format!("/tmp/mk031-mismatch-{}", std::process::id()));
    let failure = std::panic::catch_unwind(|| {
        start_daemon_for_build(dir.clone(), "deliberately-mismatched-fixture-build")
    })
    .err()
    .unwrap();
    let message = failure
        .downcast_ref::<String>()
        .map(String::as_str)
        .or_else(|| failure.downcast_ref::<&str>().copied())
        .unwrap();
    assert!(message.contains("native fixture build mismatch before GUI launch"));
    let pid = message
        .split_once("daemon PID ")
        .unwrap()
        .1
        .split(')')
        .next()
        .unwrap()
        .parse::<u32>()
        .unwrap();
    wait_for("rejected helper cleanup", || {
        !Command::new("/bin/kill")
            .args(["-0", &pid.to_string()])
            .stderr(Stdio::null())
            .status()
            .unwrap()
            .success()
    });
    assert!(
        !dir.exists(),
        "failed preparation left fixture state behind"
    );
}
