//! MK-025: `krill doctor`가 데몬이 없으면 `krill-holder --detach`로 띄워 붙고(첫 실행), 다시 부르면 같은 krilld에
//! 붙으며(재실행), 붙지 못하면 이유를 보이고 1로 끝난다. 실제 `krill-holder`·`krilld` 바이너리(`krill` 옆)를 쓴다.
//! MK-067: 고장 3종(데몬 바이너리 누락·상태 디렉터리 쓰기 불가·git 없음)과 크래시 로그 경고, `--bundle` 리댁션.

#![expect(
    clippy::unwrap_used,
    reason = "통합 테스트 헬퍼(#[test] 밖이라 clippy.toml의 테스트 허용이 닿지 않는다)"
)]

use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::Mutex;
use std::thread;

const MK: &str = env!("CARGO_BIN_EXE_krill");

/// macOS std는 `pipe()` 뒤에 따로 CLOEXEC를 건다(`pipe2` 없음). 그 사이 다른 스레드가 `krill`를 띄우면 이 파이프가
/// 그 `krill`로 새고, 그 `krill`가 holder를 띄우면 holder·krilld가 쓰기 끝을 계속 쥐어 `output()`이 EOF를 영원히
/// 기다린다(OWE-484, 부하 중 재현). 파이프 생성과 spawn을 한 스레드씩 한다.
static SPAWN: Mutex<()> = Mutex::new(());

struct StateDir(PathBuf);

impl std::ops::Deref for StateDir {
    type Target = PathBuf;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl AsRef<Path> for StateDir {
    fn as_ref(&self) -> &Path {
        &self.0
    }
}

impl AsRef<OsStr> for StateDir {
    fn as_ref(&self) -> &OsStr {
        self.0.as_os_str()
    }
}

impl Drop for StateDir {
    fn drop(&mut self) {
        kill_daemons_under(&self.0);
        fs::remove_dir_all(&self.0).ok();
    }
}

fn state_dir(name: &str) -> StateDir {
    let d = std::env::temp_dir().join(format!("mk025-{}-{name}", std::process::id()));
    fs::remove_dir_all(&d).ok();
    StateDir(d)
}

fn kill_daemons_under(dir: &Path) {
    let Ok(ps) = Command::new("/bin/ps")
        .args(["-axwwo", "pid=,command="])
        .output()
    else {
        return;
    };
    let needle = dir.to_string_lossy().into_owned();
    let alt = needle
        .strip_prefix("/private")
        .map_or_else(|| format!("/private{needle}"), str::to_owned);
    let canon = fs::canonicalize(dir)
        .ok()
        .map(|p| p.to_string_lossy().into_owned());
    let text = String::from_utf8_lossy(&ps.stdout);
    for role in ["krill-holder", "krilld"] {
        for line in text.lines() {
            let trimmed = line.trim_start();
            let Some((pid_str, cmd)) = trimmed.split_once(char::is_whitespace) else {
                continue;
            };
            let matches_dir = cmd.contains(&needle)
                || cmd.contains(&alt)
                || canon.as_deref().is_some_and(|c| cmd.contains(c));
            if matches_dir
                && cmd.contains(role)
                && let Ok(pid) = pid_str.trim().parse::<u32>()
            {
                Command::new("/bin/kill")
                    .args(["-9", &pid.to_string()])
                    .status()
                    .ok();
            }
        }
    }
}

fn doctor(state: &Path) -> Output {
    doctor_with(MK, state, &[], &[])
}

fn doctor_with(krill: &str, state: &Path, env: &[(&str, &OsStr)], args: &[&str]) -> Output {
    let bin = Path::new(MK).parent().unwrap();
    for b in ["krill-holder", "krilld"] {
        assert!(
            bin.join(b).is_file(),
            "{b} is not built next to krill (run the whole workspace: cargo nextest run --workspace)"
        );
    }
    let child = {
        let _g = SPAWN.lock().unwrap();
        Command::new(krill)
            .arg("doctor")
            .args(args)
            .env("KRILL_STATE_DIR", state)
            .envs(env.iter().copied())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap()
    };
    child.wait_with_output().unwrap()
}

fn stdout(o: &Output) -> String {
    String::from_utf8(o.stdout.clone()).unwrap()
}

/// `daemon     ok: krilld <build> instance <id> …`의 id(krilld pid).
fn instance(out: &str) -> u32 {
    let line = out.lines().find(|l| l.starts_with("daemon")).unwrap();
    let rest = line.split("instance ").nth(1).unwrap();
    rest.split(' ').next().unwrap().parse().unwrap()
}

fn ps(pid: u32, field: &str) -> String {
    let o = Command::new("/bin/ps")
        .args(["-o", &format!("{field}="), "-p", &pid.to_string()])
        .output()
        .unwrap();
    String::from_utf8(o.stdout).unwrap().trim().to_owned()
}

/// 띄운 holder·krilld를 테스트가 끝나면 죽인다.
struct Daemon {
    krilld: u32,
    holder: u32,
    dir: PathBuf,
}

impl Daemon {
    fn new(krilld: u32, dir: PathBuf) -> Self {
        let holder = ps(krilld, "ppid").parse().unwrap();
        Self {
            krilld,
            holder,
            dir,
        }
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        // holder 먼저: krilld가 먼저 죽으면 holder가 다시 띄운다.
        for pid in [self.holder, self.krilld] {
            Command::new("/bin/kill")
                .args(["-9", &pid.to_string()])
                .status()
                .ok();
        }
        kill_daemons_under(&self.dir);
        fs::remove_dir_all(&self.dir).ok();
    }
}

#[test]
fn first_run_launches_and_rerun_reuses() {
    let dir = state_dir("rerun");
    let first = doctor(&dir);
    let out = stdout(&first);
    assert!(first.status.success(), "{out}");
    assert!(out.contains("(launched)"), "{out}");
    let d = Daemon::new(instance(&out), dir.clone());
    // `/tmp`는 macOS에서 `private/tmp`로의 심링크다. 데몬은 실행 파일 경로를
    // canonicalize해 holder를 띄우므로 `ps comm`은 풀이된 경로를 보인다.
    // 비교 양쪽을 canonicalize로 맞춘다(OWE-602).
    assert_eq!(
        fs::canonicalize(ps(d.holder, "comm")).unwrap(),
        fs::canonicalize(Path::new(MK).with_file_name("krill-holder")).unwrap()
    );
    // setsid: holder는 세션 리더(STAT `s`)이고 제어 터미널이 없다.
    assert!(
        ps(d.holder, "stat").contains('s'),
        "holder is not a session leader"
    );
    assert_eq!(ps(d.holder, "tty"), "??");

    let again = doctor(&dir);
    let out = stdout(&again);
    assert!(again.status.success(), "{out}");
    assert!(!out.contains("(launched)"), "{out}");
    assert_eq!(instance(&out), d.krilld);
}

/// 동시에 첫 실행해도(앱과 `krill`가 함께 뜨는 경우) holder는 하나만 띄우고 모두 같은 krilld에 붙는다.
#[test]
fn concurrent_first_runs_launch_once() {
    let dir = state_dir("concurrent");
    let runs: Vec<_> = (0..8)
        .map(|_| {
            let dir = dir.clone();
            thread::spawn(move || doctor(&dir))
        })
        .collect();
    let outs: Vec<_> = runs.into_iter().map(|t| t.join().unwrap()).collect();
    let texts: Vec<_> = outs.iter().map(stdout).collect();
    for (o, t) in outs.iter().zip(&texts) {
        assert!(o.status.success(), "{t}");
    }
    let _d = Daemon::new(instance(&texts[0]), dir.clone());
    assert_eq!(texts.iter().filter(|t| t.contains("(launched)")).count(), 1);
    assert!(texts.iter().all(|t| instance(t) == instance(&texts[0])));
}

/// OWE-493: 감시 클라이언트가 없는 동안 잃은 holder도 초기 상태에서 감지한다.
/// krill doctor를 동시에 띄워도 새 holder는 하나이며 모두 재결합한 krilld에 붙는다.
#[test]
fn concurrent_orphan_recovery_launches_once() {
    let dir = state_dir("orphan");
    let first = doctor(&dir);
    assert!(first.status.success(), "{}", stdout(&first));
    let mut daemon = Daemon::new(instance(&stdout(&first)), dir.clone());
    let old = daemon.krilld;
    let status = Command::new("/bin/kill")
        .args(["-9", &daemon.holder.to_string()])
        .status()
        .unwrap();
    assert!(status.success());
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let handoff = dir.join("run/krilld.sock.hnd");
    while !handoff.exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "orphan did not prepare handoff"
        );
        thread::sleep(std::time::Duration::from_millis(10));
    }
    let runs: Vec<_> = (0..8)
        .map(|_| {
            let dir = dir.clone();
            thread::spawn(move || doctor(&dir))
        })
        .collect();
    let outs: Vec<_> = runs.into_iter().map(|t| t.join().unwrap()).collect();
    let texts: Vec<_> = outs.iter().map(stdout).collect();
    let recovered = texts.iter().find(|t| t.contains("daemon     ok:")).unwrap();
    daemon.krilld = instance(recovered);
    daemon.holder = ps(daemon.krilld, "ppid").parse().unwrap();
    for (o, t) in outs.iter().zip(&texts) {
        assert!(o.status.success(), "{t}");
    }
    assert_ne!(daemon.krilld, old);
    assert_eq!(texts.iter().filter(|t| t.contains("(launched)")).count(), 1);
    assert!(texts.iter().all(|t| instance(t) == daemon.krilld));
    assert!(ps(daemon.holder, "stat").contains('s'));
}

#[test]
fn failure_is_reported() {
    let dir = state_dir(&"x".repeat(100));
    let o = doctor(&dir);
    let out = stdout(&o);
    assert_eq!(o.status.code(), Some(1), "{out}");
    assert!(
        out.contains("daemon     FAIL: socket path too long"),
        "{out}"
    );
}

/// 고장 ①: `krill` 옆에 `krill-holder`·`krilld`가 없다(복사·설치 실수). 데몬을 띄우려 하지 않는다.
#[test]
fn missing_daemon_binaries_are_reported() {
    let dir = state_dir("nobin");
    let lone = dir.join("bin/krill");
    fs::create_dir_all(lone.parent().unwrap()).unwrap();
    fs::copy(MK, &lone).unwrap();
    let o = doctor_with(lone.to_str().unwrap(), &dir.join("state"), &[], &[]);
    let out = stdout(&o);
    assert_eq!(o.status.code(), Some(1), "{out}");
    assert!(out.contains("krill-holder FAIL: missing"), "{out}");
    assert!(out.contains("krilld     FAIL: missing"), "{out}");
    assert!(out.contains("daemon     SKIP"), "{out}");
    fs::remove_dir_all(&dir).ok();
}

/// 고장 ②: 상태 디렉터리에 쓸 수 없다(예: `sudo`로 만들어져 소유자가 다름).
#[test]
fn unwritable_state_dir_is_reported() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = state_dir("ro");
    fs::create_dir_all(&dir).unwrap();
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o500)).unwrap();
    let o = doctor(&dir);
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
    let out = stdout(&o);
    assert_eq!(o.status.code(), Some(1), "{out}");
    assert!(out.contains("state      FAIL:"), "{out}");
    assert!(out.contains("daemon     SKIP"), "{out}");
    fs::remove_dir_all(&dir).ok();
}

/// 고장 ③에서 권고로 변경(OWE-689): PATH에 git이 없어도 데몬 진단은 그대로 하고, 확인하지 않은
/// 크래시 로그는 경고로 보인다. git 없음은 `warn`이라 종료 코드는 0이다.
#[test]
fn missing_git_and_crash_logs_are_reported() {
    let dir = state_dir("nogit");
    let crash = dir.join("crash");
    fs::create_dir_all(&crash).unwrap();
    fs::write(crash.join("crash-1-1-krill-app.txt"), "message: boom").unwrap();
    let empty = dir.join("empty-path");
    fs::create_dir_all(&empty).unwrap();
    let o = doctor_with(MK, &dir, &[("PATH", empty.as_os_str())], &[]);
    let out = stdout(&o);
    let _d = out
        .contains("daemon     ok:")
        .then(|| Daemon::new(instance(&out), dir.clone()));
    assert!(o.status.success(), "{out}");
    assert!(out.contains("git        WARN:"), "{out}");
    assert!(out.contains("crash      WARN: 1 unreported"), "{out}");
    assert!(out.contains("daemon     ok:"), "{out}");
}

#[test]
fn bundle_masks_secrets() {
    let dir = state_dir("bundle");
    let crash = dir.join("crash");
    fs::create_dir_all(&crash).unwrap();
    fs::write(
        crash.join("crash-1-1-krill-app.txt"),
        "message: ANTHROPIC_API_KEY=sk-ant-api03-zzzz ghp_abcdefghijklmnopqrstuvwxyz012345\n\
         -----BEGIN EC PRIVATE KEY-----\nMHcCAQEEIBkey\n-----END EC PRIVATE KEY-----\n",
    )
    .unwrap();
    let o = doctor_with(MK, &dir, &[], &["--bundle"]);
    let out = stdout(&o);
    assert!(o.status.success(), "{out}");
    let text = fs::read_to_string(out.trim()).unwrap();
    for leak in ["sk-ant-api03", "ghp_abc", "MHcCAQEEIBkey"] {
        assert!(!text.contains(leak), "{leak} leaked:\n{text}");
    }
    assert!(text.contains("[REDACTED PEM]"), "{text}");
    // 번들은 크래시 로그를 확인 처리하지 않는다(대화상자만 한다).
    assert!(crash.join("crash-1-1-krill-app.txt").exists());
    fs::remove_dir_all(&dir).ok();
}

/// OWE-580 (P1-10): 온보딩이 만드는 `~/.local/bin/krill` 같은 심볼릭 링크로 실행해도
/// 데몬 바이너리를 찾아 기동하고 `krill doctor`도 통과한다(앱 번들 `Contents/Helpers` 배치 포함).
#[test]
fn symlink_launches_and_passes_doctor() {
    use std::os::unix::fs::symlink;

    let dir = state_dir("symlink");
    let link_dir = dir.join("local-bin");
    fs::create_dir_all(&link_dir).unwrap();
    let link = link_dir.join("krill");
    symlink(MK, &link).unwrap();

    let state = dir.join("state");
    let child = {
        let _g = SPAWN.lock().unwrap();
        Command::new(&link)
            .arg("ls")
            .env("KRILL_STATE_DIR", &state)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap()
    };
    let ls = child.wait_with_output().unwrap();
    assert!(
        ls.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&ls.stdout),
        String::from_utf8_lossy(&ls.stderr)
    );

    let doc = doctor_with(link.to_str().unwrap(), &state, &[], &[]);
    let out = stdout(&doc);
    let d = Daemon::new(instance(&out), state);
    assert!(doc.status.success(), "{out}");
    assert!(out.contains("daemon     ok:"), "{out}");

    // 앱 번들(`Contents/MacOS/krill` + `Contents/Helpers/{krill-holder,krilld}`)을 가리키는 심볼릭 링크.
    let bundle = dir.join("Krill.app/Contents");
    fs::create_dir_all(bundle.join("MacOS")).unwrap();
    fs::create_dir_all(bundle.join("Helpers")).unwrap();
    let real_bin = Path::new(MK).parent().unwrap();
    fs::copy(MK, bundle.join("MacOS/krill")).unwrap();
    symlink(
        real_bin.join("krill-holder"),
        bundle.join("Helpers/krill-holder"),
    )
    .unwrap();
    symlink(real_bin.join("krilld"), bundle.join("Helpers/krilld")).unwrap();
    let bundle_link = link_dir.join("mk-bundle");
    symlink(bundle.join("MacOS/krill"), &bundle_link).unwrap();

    let bundle_state = dir.join("bundle-state");
    let doc2 = doctor_with(bundle_link.to_str().unwrap(), &bundle_state, &[], &[]);
    let out2 = stdout(&doc2);
    let d2 = Daemon::new(instance(&out2), bundle_state);
    assert!(doc2.status.success(), "{out2}");
    assert!(out2.contains("Helpers/krill-holder"), "{out2}");
    assert!(out2.contains("Helpers/krilld"), "{out2}");
    drop(d);
    drop(d2);
    fs::remove_dir_all(&dir).ok();
}

/// OWE-580: `krill doctor | head -1`처럼 파이프가 일찍 닫혀 EPIPE가 나도 panic(101) 없이 0으로 끝난다.
#[test]
fn broken_pipe_exits_zero() {
    use std::io::{BufRead as _, BufReader};

    let dir = state_dir("epipe");
    let first = doctor(&dir);
    let out = stdout(&first);
    assert!(first.status.success(), "{out}");
    let _d = Daemon::new(instance(&out), dir.clone());

    for read_first_line in [false, true] {
        let mut child = {
            let _g = SPAWN.lock().unwrap();
            Command::new(MK)
                .arg("doctor")
                .env("KRILL_STATE_DIR", &dir)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap()
        };
        if read_first_line {
            let mut reader = BufReader::new(child.stdout.take().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            assert!(line.starts_with("socket"), "{line}");
        } else {
            drop(child.stdout.take());
        }
        let done = child.wait_with_output().unwrap();
        let err = String::from_utf8_lossy(&done.stderr);
        assert!(
            done.status.success(),
            "status={:?} stderr={err}",
            done.status.code()
        );
        assert!(!err.contains("panicked"), "{err}");
    }
}
