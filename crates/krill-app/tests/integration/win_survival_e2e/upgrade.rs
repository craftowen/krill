//! 업데이트 뒤 업그레이드 요청이 Windows 세션을 잃지 않는다(window-error 회귀).
//!
//! 실제 사용자 설치본처럼 `conpty.dll`·`x64\OpenConsole.exe`가 holder 옆에 있는 번들 모드에서, 업데이터의
//! Rename 교체(설치 폴더 → `.prev`, `.staging` → 설치 폴더, 실행 중 이미지는 이름만 바뀐다)를 흉내 낸 뒤
//! `daemon::upgrade_as`로 더 새 빌드를 요청한다.
//! - 기본: 세션이 살아 있으면 holder는 교체하지 않고 krilld만 새 이미지로 바뀐다. 수정 전에는 holder가 새 프로세스로
//!   핸드오프돼 holder PID 단언이 실패한다.
//! - `KRILL_E2E_OLD_BIN`(수정 전 릴리스의 holder·krilld·ConPTY 폴더)을 주면 구 데몬이 처음 띄워지고, 새 클라이언트가
//!   업그레이드를 보류해야 한다(holder·krilld 모두 그대로).

use super::*;

const BUNDLE: [&str; 4] = [
    "krill-holder.exe",
    "krilld.exe",
    "conpty.dll",
    r"x64\OpenConsole.exe",
];

fn copy_bundle(from: &Path, to: &Path) {
    for name in BUNDLE {
        let src = from.join(name);
        assert!(
            src.is_file(),
            "{} missing: KRILL_E2E_BIN/KRILL_E2E_OLD_BIN need the bundled ConPTY (packaging/windows/build.ps1)",
            src.display()
        );
        let dst = to.join(name);
        fs::create_dir_all(dst.parent().unwrap()).unwrap();
        fs::copy(&src, &dst).unwrap();
    }
}

/// 업데이터 `rename::move_tree`와 같은 파일 단위 이동(빈 대상 폴더 전제라 치우기 단계는 없다).
fn move_tree(src: &Path, dst: &Path) {
    fs::create_dir_all(dst).unwrap();
    for entry in fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let to = dst.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            move_tree(&entry.path(), &to);
            fs::remove_dir(entry.path()).ok();
        } else {
            fs::rename(entry.path(), &to).unwrap();
        }
    }
}

/// 실행 파일이 `under` 아래에 있는 `name` 프로세스 PID들.
fn images(name: &str, under: &Path) -> Vec<u32> {
    // 작은따옴표 문자열 안이라 `'`만 겹쳐 쓴다. 와일드카드(`-like`) 대신 접두 비교라 `[`·`*`도 그대로다.
    let prefix = format!("{}\\", under.display()).replace('\'', "''");
    let q = format!(
        "Get-CimInstance Win32_Process -Filter \"Name = '{name}'\" | Where-Object {{ $_.ExecutablePath -and $_.ExecutablePath.StartsWith('{prefix}', 'OrdinalIgnoreCase') }} | ForEach-Object {{ $_.ProcessId }}"
    );
    let o = Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", &q])
        .output()
        .unwrap();
    assert!(
        o.status.success(),
        "process query failed: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    let mut pids: Vec<u32> = String::from_utf8_lossy(&o.stdout)
        .lines()
        .filter_map(|l| l.trim().parse().ok())
        .collect();
    pids.sort_unstable();
    pids
}

#[test]
#[ignore = "Windows 비밀번호 로그온 + release holder·krilld + 번들 ConPTY 필요(window-error)"]
fn rename_update_keeps_live_holder() {
    let old_bin = env::var_os("KRILL_E2E_OLD_BIN").map(PathBuf::from);
    let mut rig = Rig::new("upgrade");
    let install = rig.dir.join("Krill");
    let staging = rig.dir.join("Krill.staging");
    let prev = rig.dir.join("Krill.prev");
    copy_bundle(old_bin.as_deref().unwrap_or(&bin_dir()), &install);
    copy_bundle(&bin_dir(), &staging);
    rig.paths.holder = install.join("krill-holder.exe");
    rig.paths.krilld = install.join("krilld.exe");

    let (holder, krilld) = rig.launch();
    let mut c = rig.control();
    let (session, pid) = create(&mut c, &rig.dir);
    rig.pids.push(pid);
    drop(c);
    // 번들 모드 확인: 수정 전 holder가 세션째 핸드오프하던 경로다(inbox 모드는 원래 EBUSY).
    let consoles = images("OpenConsole.exe", &install);
    assert!(!consoles.is_empty(), "bundled OpenConsole not in use");
    let live = follow(&rig.paths, session);
    type_line(&rig.paths, session, &gen_command("U,0,50,10"));
    wait_live(&live, "generator start", |b| contains(b, b"U0000100-"));

    move_tree(&install, &prev);
    move_tree(&staging, &install);
    let before = live.lock().unwrap().bytes.len();
    let upgraded = daemon::upgrade_as(
        &rig.paths,
        &daemon::client_hello(ClientRole::Cli, "owe836-upgrade"),
        "999.0.0",
    )
    .unwrap();
    let new_krilld = match (&old_bin, upgraded) {
        // 구 데몬(FEAT_WINDOWS_UPGRADE 없음): 조회 경합 없이 무조건 보류.
        (Some(_), None) => krilld,
        (None, Some(server)) => u32::try_from(server.instance_id).unwrap(),
        (old, got) => panic!("upgrade policy: old_bin={old:?} got={got:?}"),
    };
    rig.pids.push(new_krilld);
    if old_bin.is_none() {
        assert_ne!(new_krilld, krilld, "krilld not replaced");
        assert!(wait_dead(krilld, WAIT), "old krilld did not retire");
        assert_eq!(
            images("krilld.exe", &install),
            vec![new_krilld],
            "new krilld not from the updated install"
        );
    }
    // 핵심 정책: 활성 세션의 holder는 같은 프로세스로 남고 다른 holder가 생기지 않는다.
    assert!(alive(holder), "holder replaced during update");
    assert_eq!(parent(new_krilld), holder, "krilld not owned by old holder");
    assert_eq!(
        images("krill-holder.exe", &rig.dir),
        vec![holder],
        "extra holder"
    );
    assert!(alive(pid), "session died during update");
    for p in &consoles {
        assert!(alive(*p), "OpenConsole {p} died during update");
    }
    wait_live(&live, "output after update", |b| b.len() > before + 16_384);

    fs::write(rig.dir.join("stop"), b"").unwrap();
    wait_live(&live, "generator end", |b| {
        end_count(&plain(b), b'U').is_some()
    });
    let mut c = rig.control();
    type_line(&rig.paths, session, "echo UPG_%COMPUTERNAME:~0,0%INPUT_OK");
    wait_screen(&mut c, session, "input after update", |s| {
        has_line(s, "UPG_INPUT_OK")
    });
    thread::sleep(Duration::from_millis(300));
    let (full, gaps) = replay(&rig.paths, session);
    // 재생은 실시간 구독보다 앞설 수 있다. 같은 길이까지 따라잡은 뒤 비교한다.
    wait_live(&live, "live catch-up", |b| b.len() >= full.len());
    let live_bytes = live.lock().unwrap().bytes.clone();
    let (live_gaps, reconnects) = {
        let l = live.lock().unwrap();
        (l.gaps.len(), l.reconnects)
    };
    let total = end_count(&plain(&live_bytes), b'U').unwrap();
    let (seen, bad) = numbers(&plain(&full), b'U');
    say(&format!(
        "RESULT upgrade old_bin={} holder={holder} krilld={krilld}->{new_krilld} lines={total} bytes={} \
         gaps={} missing={} bad_tag={bad} live_gaps={} reconnects={}",
        old_bin.is_some(),
        full.len(),
        gaps.len(),
        missing(&seen, total),
        live_gaps,
        reconnects,
    ));
    assert!(
        gaps.is_empty() && missing(&seen, total) == 0 && bad == 0,
        "output lost across upgrade"
    );
    assert_eq!(full, live_bytes[..full.len()], "upgrade SHA-256 mismatch");
    assert_eq!(live_gaps, 0, "live stream Gap");

    // 구 holder(`.prev` 이미지)가 새 설치 폴더의 ConPTY로 새 세션을 만든다.
    let (created, new_pid) = create(&mut c, &rig.dir);
    rig.pids.push(new_pid);
    type_line(
        &rig.paths,
        created,
        "echo NEW_%COMPUTERNAME:~0,0%SESSION_OK",
    );
    wait_screen(&mut c, created, "new session input", |s| {
        has_line(s, "NEW_SESSION_OK")
    });
    for (id, process) in [(session, pid), (created, new_pid)] {
        assert!(matches!(
            request(
                &mut c,
                &Request::Resize {
                    session: id,
                    cols: 90,
                    rows: 30
                }
            ),
            Reply::Resized
        ));
        assert!(matches!(
            request(&mut c, &Request::Close { session: id }),
            Reply::Closed
        ));
        assert!(wait_dead(process, WAIT), "session survived close");
    }
    drop(c);
    let strays = [rig.strays("krill-holder.exe"), rig.strays("krilld.exe")].concat();
    assert!(strays.is_empty(), "unexpected processes {strays:?}");
}
