use super::*;
use std::fmt::Write as _;
use std::os::unix::process::ExitStatusExt as _;
use std::sync::atomic::{AtomicUsize, Ordering};

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "kl-lifecycle-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).ok();
    }
}

fn output(text: &str, success: bool) -> Output {
    Output {
        status: std::process::ExitStatus::from_raw(if success { 0 } else { 256 }),
        stdout: text.as_bytes().to_vec(),
        stderr: Vec::new(),
    }
}

#[test]
fn overridden_home_never_qualifies_for_default_autostart() {
    let real = Fixture::new();
    let temporary = Fixture::new();
    assert!(same_home(Some(&real.0), Some(&real.0)));
    assert!(!same_home(Some(&temporary.0), Some(&real.0)));
    assert!(!same_home(Some(&real.0), None));
    assert!(!same_home(None, Some(&real.0)));
    let alias = temporary.0.join("home-alias");
    std::os::unix::fs::symlink(&real.0, &alias).unwrap();
    assert!(same_home(Some(&alias), Some(&real.0)));
}

#[test]
fn cleanup_only_boots_out_hashed_agents_with_confirmed_missing_paths() {
    let fixture = Fixture::new();
    let orphan = format!("{LINK_LABEL}.1111111111111111");
    let healthy = format!("{LINK_LABEL}.2222222222222222");
    let unknown = format!("{LINK_LABEL}.3333333333333333");
    let mismatched = format!("{LINK_LABEL}.4444444444444444");
    let failed = format!("{LINK_LABEL}.5555555555555555");
    let malformed = format!("{LINK_LABEL}.bad");
    let deleted_state = fixture.0.join("deleted-state");
    let state_orphan = link_label(Some(&deleted_state));
    let state_orphan_plist = fixture.0.join(format!("{state_orphan}.plist"));
    fs::write(&state_orphan_plist, "state orphan").unwrap();
    let healthy_path = fixture.0.join(format!("{healthy}.plist"));
    fs::write(&healthy_path, "healthy").unwrap();
    let list = [
        &orphan,
        &state_orphan,
        &healthy,
        &unknown,
        &mismatched,
        &failed,
        &malformed,
        LINK_LABEL,
        "other.agent",
    ]
    .into_iter()
    .fold(String::new(), |mut list, label| {
        writeln!(list, "1\t0\t{label}").unwrap();
        list
    });
    let mut booted = Vec::new();
    let mut inspected = Vec::new();
    cleanup_orphans(&mut |args| match args[0].as_str() {
        "list" => Ok(output(&list, true)),
        "print" => {
            let target = &args[1];
            let label = target.rsplit('/').next().unwrap();
            inspected.push(label.to_owned());
            if label == unknown {
                let mut bytes = format!("{target} = {{\n\tpath = /tmp/").into_bytes();
                bytes.push(0xff);
                bytes.extend_from_slice(
                    format!("/{label}.plist\n\ttype = LaunchAgent\n}}\n").as_bytes(),
                );
                return Ok(Output {
                    stdout: bytes,
                    ..output("", true)
                });
            }
            if label == failed {
                return Ok(output("", false));
            }
            let file_label = if label == mismatched { &orphan } else { label };
            let environment = if label == state_orphan {
                format!(
                    "\tenvironment = {{\n\t\tKRILL_STATE_DIR => {}\n\t}}\n",
                    deleted_state.display()
                )
            } else {
                String::new()
            };
            Ok(output(
                &format!(
                    "{target} = {{\n\tpath = {}\n\ttype = LaunchAgent\n{environment}}}\n",
                    fixture.0.join(format!("{file_label}.plist")).display()
                ),
                true,
            ))
        }
        "bootout" => {
            booted.push(args[1].clone());
            Ok(output("", true))
        }
        other => panic!("unexpected launchctl command: {other}"),
    });
    assert_eq!(
        booted,
        [
            format!("gui/{}/{orphan}", rustix::process::getuid().as_raw()),
            format!("gui/{}/{state_orphan}", rustix::process::getuid().as_raw())
        ]
    );
    assert!(
        !inspected
            .iter()
            .any(|label| label == LINK_LABEL || label == &malformed)
    );
    assert_eq!(fs::read_to_string(healthy_path).unwrap(), "healthy");
}

#[test]
fn own_agent_retires_on_state_or_plist_loss_and_retries_failed_bootout() {
    for removal in 0..3 {
        let fixture = Fixture::new();
        let state = fixture.0.join("state");
        fs::create_dir(&state).unwrap();
        let label = link_label(Some(&state));
        let plist = fixture.0.join(format!("{label}.plist"));
        fs::write(&plist, "plist").unwrap();
        let agent = ManagedAgent::new(&label, &state, plist.clone()).unwrap();
        let mut calls = Vec::new();
        assert!(!agent.retire(&mut |_| panic!("healthy agent must be untouched")));
        match removal {
            0 => fs::remove_file(&plist).unwrap(),
            1 => fs::remove_dir(&state).unwrap(),
            _ => {
                fs::remove_dir(&state).unwrap();
                fs::create_dir(&state).unwrap();
            }
        }
        assert!(agent.is_orphaned());
        assert!(!agent.retire(&mut |args| {
            calls.push(args.to_vec());
            Ok(output("bootout failed", false))
        }));
        assert!(agent.retire(&mut |args| {
            calls.push(args.to_vec());
            Ok(output("", true))
        }));
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0], ["bootout".to_owned(), agent.target.clone()]);
        assert_eq!(calls[0], calls[1]);
    }
}

#[test]
fn unmounted_volume_is_unavailable_and_never_booted_out() {
    let fixture = Fixture::new();
    let state = PathBuf::from(format!(
        "/Volumes/krill-not-mounted-{}/state",
        std::process::id()
    ));
    assert!(!state.exists());
    assert_eq!(path_state(&state), PathState::Unavailable);
    for path in [
        "/Network/krill-missing/state",
        "/net/krill-missing/state",
        "/home/krill-missing/state",
    ] {
        assert_eq!(path_state(Path::new(path)), PathState::Unavailable);
    }
    let label = link_label(Some(&state));
    // plist 부재보다 상태의 접근 불가가 우선한다.
    let plist = fixture.0.join(format!("{label}.plist"));
    let agent = ManagedAgent::new(&label, &state, plist.clone()).unwrap();
    assert!(!agent.is_orphaned());
    let mut count = 1;
    assert!(
        agent
            .observe(&mut count, &mut |_| panic!(
                "unmounted agent must be preserved"
            ))
            .is_err()
    );
    assert_eq!(count, 0);
    cleanup_orphans(&mut |args| match args[0].as_str() {
        "list" => Ok(output(&format!("1\t0\t{label}\n"), true)),
        "print" => Ok(output(
            &format!(
                "{} = {{\n\ttype = LaunchAgent\n\tpath = {}\n\t\tKRILL_STATE_DIR => {}\n}}\n",
                args[1],
                plist.display(),
                state.display()
            ),
            true,
        )),
        _ => panic!("unmounted agent must not be booted out"),
    });
    assert_eq!(
        path_state(&fixture.0.join("missing/state")),
        PathState::Missing
    );
    // ENOTDIR 등 NotFound 이외의 실패도 삭제 근거로 사용하지 않는다.
    let file = fixture.0.join("file");
    fs::write(&file, "file").unwrap();
    assert_eq!(path_state(&file.join("state")), PathState::Unavailable);
}

fn assert_unavailable_agent_is_preserved(state: &Path) {
    let fixture = Fixture::new();
    assert_eq!(path_state(state), PathState::Unavailable);
    let label = link_label(Some(state));
    let plist = fixture.0.join(format!("{label}.plist"));
    for present in [false, true] {
        if present {
            fs::write(&plist, "plist").unwrap();
        }
        let agent = ManagedAgent::new(&label, state, plist.clone()).unwrap();
        let mut count = 1;
        assert!(
            agent
                .observe(&mut count, &mut |_| panic!("unexpected bootout"))
                .is_err()
        );
        assert_eq!(count, 0);
        assert!(!agent.is_orphaned());
        cleanup_orphans(&mut |args| match args[0].as_str() {
            "list" => Ok(output(&format!("1\t0\t{label}\n"), true)),
            "print" => Ok(output(
                &format!(
                    "{} = {{\n\ttype = LaunchAgent\n\tpath = {}\n\t\tKRILL_STATE_DIR => {}\n}}\n",
                    args[1],
                    plist.display(),
                    state.display()
                ),
                true,
            )),
            _ => panic!("unavailable agent must not be booted out"),
        });
    }
}

#[test]
fn symlink_to_unmounted_volume_is_unavailable() {
    let fixture = Fixture::new();
    let volume = PathBuf::from(format!("/Volumes/krill-unmounted-{}", std::process::id()));
    assert!(!volume.exists());
    let alias = fixture.0.join("external");
    std::os::unix::fs::symlink(&volume, &alias).unwrap();
    assert_unavailable_agent_is_preserved(&alias);
    assert_unavailable_agent_is_preserved(&alias.join("state"));
    let volumes_alias = fixture.0.join("volumes");
    std::os::unix::fs::symlink("/Volumes", &volumes_alias).unwrap();
    assert_unavailable_agent_is_preserved(
        &volumes_alias
            .join(volume.file_name().unwrap())
            .join("state"),
    );
}

#[test]
fn lowercase_volumes_alias_is_unavailable() {
    // 대소문자를 구분하는 파일시스템에는 이 별칭 자체가 없다.
    if !Path::new("/volumes").exists() {
        return;
    }
    let state = PathBuf::from(format!(
        "/volumes/krill-unmounted-{}/state",
        std::process::id()
    ));
    assert_unavailable_agent_is_preserved(&state);
}

#[test]
fn firmlink_volumes_alias_is_unavailable() {
    let state = PathBuf::from(format!(
        "/System/Volumes/Data/Volumes/krill-unmounted-{}/state",
        std::process::id()
    ));
    assert_unavailable_agent_is_preserved(&state);
}

#[test]
fn missing_tmp_state_with_existing_plist_is_preserved_at_start_and_cleaned_in_background() {
    let fixture = Fixture::new();
    let state = Path::new("/tmp")
        .join(fixture.0.file_name().unwrap())
        .join("gone");
    assert_eq!(path_state(&state), PathState::Missing);
    let label = link_label(Some(&state));
    let plist = fixture.0.join(format!("{label}.plist"));
    fs::write(&plist, "plist").unwrap();
    let agent = ManagedAgent::new(&label, &state, plist.clone()).unwrap();
    let mut count = 0;
    assert!(
        agent
            .observe(&mut count, &mut |_| panic!("startup must keep retrying"))
            .is_err()
    );
    let mut prints = 0;
    let mut booted = Vec::new();
    cleanup_orphans(&mut |args| match args[0].as_str() {
        "list" => Ok(output(&format!("1\t0\t{label}\n"), true)),
        "print" => {
            prints += 1;
            Ok(output(
                &format!(
                    "{} = {{\n\ttype = LaunchAgent\n\tpath = {}\n\t\tKRILL_STATE_DIR => {}\n}}\n",
                    args[1],
                    plist.display(),
                    state.display()
                ),
                true,
            ))
        }
        "bootout" => {
            booted.push(args[1].clone());
            Ok(output("", true))
        }
        _ => panic!("unexpected command"),
    });
    assert_eq!(prints, 2);
    assert_eq!(booted, [agent.target]);
    assert!(!state.exists());
    assert!(plist.exists());
}

#[test]
fn runtime_filesystem_change_is_unavailable_instead_of_orphaned() {
    let fixture = Fixture::new();
    let state = fixture.0.join("state");
    fs::create_dir(&state).unwrap();
    let label = link_label(Some(&state));
    let plist = fixture.0.join(format!("{label}.plist"));
    fs::write(&plist, "plist").unwrap();
    let agent = ManagedAgent::new(&label, &state, plist).unwrap();
    assert_ne!(
        fs::metadata(&state).unwrap().dev(),
        fs::metadata("/dev").unwrap().dev()
    );
    fs::remove_dir(&state).unwrap();
    // 실제 mount/unmount 없이 다른 파일시스템이 경로에 보이는 경우를 재현한다.
    std::os::unix::fs::symlink("/dev", &state).unwrap();
    assert_eq!(agent.state_status(), PathState::Unavailable);
    assert!(!agent.is_orphaned());
    let mut count = 1;
    assert!(
        agent
            .observe(&mut count, &mut |_| panic!(
                "filesystem change must not bootout"
            ))
            .is_err()
    );
}

#[test]
fn removal_requires_two_consecutive_observations_and_recovery_resets_count() {
    let fixture = Fixture::new();
    let label = link_label(Some(&fixture.0));
    let plist = fixture.0.join(format!("{label}.plist"));
    fs::write(&plist, "plist").unwrap();
    let agent = ManagedAgent::new(&label, &fixture.0, plist.clone()).unwrap();
    let mut count = 0;
    let mut calls = 0;
    let mut run = |_: &[String]| {
        calls += 1;
        Ok(output("", true))
    };
    fs::remove_file(&plist).unwrap();
    assert_eq!(
        agent.observe(&mut count, &mut run).unwrap(),
        Observation::Pending
    );
    fs::write(&plist, "restored").unwrap();
    assert_eq!(
        agent.observe(&mut count, &mut run).unwrap(),
        Observation::Healthy
    );
    fs::remove_file(&plist).unwrap();
    assert_eq!(
        agent.observe(&mut count, &mut run).unwrap(),
        Observation::Pending
    );
    assert_eq!(
        agent.observe(&mut count, &mut run).unwrap(),
        Observation::Retired
    );
    assert_eq!(calls, 1);
}

#[test]
fn background_cleanup_preserves_plist_restored_between_observations() {
    let fixture = Fixture::new();
    let label = link_label(Some(&fixture.0));
    let plist = fixture.0.join(format!("{label}.plist"));
    let mut prints = 0;
    cleanup_orphans(&mut |args| match args[0].as_str() {
        "list" => Ok(output(&format!("1\t0\t{label}\n"), true)),
        "print" => {
            prints += 1;
            if prints == 2 {
                fs::write(&plist, "restored").unwrap();
            }
            Ok(output(
                &format!(
                    "{} = {{\n\ttype = LaunchAgent\n\tpath = {}\n}}\n",
                    args[1],
                    plist.display()
                ),
                true,
            ))
        }
        _ => panic!("restored agent must not be booted out"),
    });
    assert_eq!(prints, 2);
}

#[test]
fn management_requires_own_label_and_absolute_matching_plist() {
    let fixture = Fixture::new();
    let state = &fixture.0;
    let plist = state.join(format!("{LINK_LABEL}.plist"));
    assert!(ManagedAgent::new(LINK_LABEL, state, plist.clone()).is_some());
    assert!(ManagedAgent::new("other.agent", state, plist).is_none());
    assert!(ManagedAgent::new(LINK_LABEL, state, state.join("other.plist")).is_none());
    assert!(ManagedAgent::new(LINK_LABEL, state, PathBuf::from("relative.plist")).is_none());
    assert!(ManagedAgent::new(LINK_LABEL, Path::new("relative"), state.join("x")).is_none());
}

#[test]
fn unit_tests_cannot_run_real_launchctl() {
    assert!(!launchctl_allowed());
    assert!(ManagedAgent::from_env(Path::new("/unused")).is_none());
    cleanup_orphans_in_background();
}
