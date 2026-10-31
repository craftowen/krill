//! MK-066: 메모리 회귀 게이트. 실제 release 앱 + krilld + holder에 터미널 10개를 붙여 출력을 흘린 뒤
//! `phys_footprint` 합이 200MiB(ARCHITECTURE §출시 차단선)를 넘으면 실패한다. 세션 프로세스(에이전트 대역)는 예산 밖이다.
//! 병렬 부하에서 값이 흔들리므로 `scripts/memory-gate.sh`로만 돈다. gpui 잠정 결정 의존은 MK-031처럼 이 모듈에만 있다.
//! 재현·측정값은 비공개 MK-066 테스트 기록.

use std::fmt::Write as _;
use std::fs;

use krill_app::layout::{SavedWindowBounds, SavedWindowMode};
use krill_proto::control::{Create, Reply, Request};

use super::release_survival::{Processes, launch_app, request, start_daemon, wait_for};
use super::*;

const TERMINALS: usize = 10;
/// 세션마다 내는 줄 수(약 125B/줄 → 약 2.5MB). 핫 스크롤백 1,000행을 다 채우고 콜드 저장소도 쓰게 한다.
const GEN_LINES: usize = 20_000;
const APP_LIMIT: u64 = 120 << 20;
const DAEMON_LIMIT: u64 = 60 << 20;
const LIMIT: u64 = 200 << 20;
/// MK-009처럼 출력이 끝난 뒤 이만큼 쉬고 잰다.
const IDLE: Duration = Duration::from_secs(10);
const WINDOW_WIDTH: f32 = 1600.;
const WINDOW_HEIGHT: f32 = 1000.;

// Read the owned test process only; do not resize windows or change display settings.
// CGWindow bounds and NSScreen backing scale use the same APIs as computer/capture/geometry.js.
const WINDOW_PROBE: &str = r"
ObjC.import('AppKit');
ObjC.import('CoreGraphics');
function run(argv) {
  const sessionRaw = $.CGSessionCopyCurrentDictionary();
  if (!sessionRaw) throw Error('console session unavailable');
  const session = ObjC.deepUnwrap(ObjC.castRefToObject(sessionRaw));
  const raw = $.CGWindowListCopyWindowInfo(1, 0);
  if (!raw) throw Error('window census unavailable');
  const rows = ObjC.deepUnwrap(ObjC.castRefToObject(raw));
  const screens = $.NSScreen.screens;
  const count = Number(screens.count);
  if (count < 1 || count > 16) throw Error('display census unavailable');
  return JSON.stringify({interactive: session.CGSSessionScreenIsLocked !== true &&
    session.kCGSSessionOnConsoleKey === true,
    frontmost_pid: Number($.NSWorkspace.sharedWorkspace.frontmostApplication.processIdentifier) || 0,
    windows: rows.filter(w =>
    Number(w.kCGWindowOwnerPID) === Number(argv[0]) && Number(w.kCGWindowLayer) === 0
  ).map(w => {
    const b = w.kCGWindowBounds;
    const matches = [];
    for (let i = 0; i < count; i++) {
      const s = screens.objectAtIndex(i);
      const id = Number(ObjC.unwrap(s.deviceDescription.objectForKey('NSScreenNumber')));
      const f = $.CGDisplayBounds(id);
      if (b.X >= f.origin.x && b.Y >= f.origin.y &&
          b.X + b.Width <= f.origin.x + f.size.width &&
          b.Y + b.Height <= f.origin.y + f.size.height) {
        matches.push(Number(s.backingScaleFactor));
      }
    }
    return {width: Number(b.Width), height: Number(b.Height),
      scale: matches.length === 1 ? matches[0] : null};
  })});
}
";

#[derive(Debug, serde::Deserialize)]
struct WindowCensus {
    windows: Vec<MeasuredWindow>,
}

#[derive(Debug, serde::Deserialize)]
struct PresentationCensus {
    interactive: bool,
    frontmost_pid: u32,
    #[serde(flatten)]
    geometry: WindowCensus,
}

fn validate_console(census: &PresentationCensus, app_pid: u32) -> Result<(), &'static str> {
    if !census.interactive {
        return Err("requires an unlocked, active macOS console session");
    }
    if census.frontmost_pid != app_pid {
        return Err("requires the fixture app to remain frontmost");
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, serde::Deserialize)]
struct MeasuredWindow {
    width: f64,
    height: f64,
    scale: Option<f64>,
}

#[derive(Clone, Copy, Debug, PartialEq, serde::Deserialize)]
struct NativeWindow {
    pid: u32,
    width: f64,
    height: f64,
    scale: f64,
    window_count: usize,
    windowed: bool,
}

fn latest_geometry(dir: &Path) -> Option<PathBuf> {
    fs::read_dir(dir)
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with("memory-window-"))
                && path.extension().is_some_and(|ext| ext == "json")
        })
        .max()
}

fn validate_window(census: &WindowCensus) -> Result<&MeasuredWindow, &'static str> {
    let [window] = census.windows.as_slice() else {
        return Err("requires exactly one visible main window owned by the fixture app");
    };
    if (window.width - f64::from(WINDOW_WIDTH)).abs() > 0.5
        || (window.height - f64::from(WINDOW_HEIGHT)).abs() > 0.5
        || !window.width.is_finite()
        || !window.height.is_finite()
    {
        return Err("requires actual 1600x1000pt window bounds");
    }
    if !window
        .scale
        .is_some_and(|scale| scale.is_finite() && (scale - 2.).abs() < 0.001)
    {
        return Err("requires the entire window on one measured @2x display");
    }
    Ok(window)
}

fn assert_window_conditions(guard: &Processes, preparing: bool) -> NativeWindow {
    wait_for("native window geometry observation", || {
        latest_geometry(&guard.dir).is_some()
    });
    let native: NativeWindow =
        serde_json::from_slice(&fs::read(latest_geometry(&guard.dir).unwrap()).unwrap()).unwrap();
    assert_eq!(
        native.pid,
        guard.apps[0].id(),
        "geometry belongs to another process"
    );
    assert_eq!(native.window_count, 1, "requires one native window");
    assert!(
        native.windowed,
        "requires a normal window, not a fullscreen/maximized restore frame"
    );
    let native_census = WindowCensus {
        windows: vec![MeasuredWindow {
            width: native.width,
            height: native.height,
            scale: Some(native.scale),
        }],
    };
    assert!(
        validate_window(&native_census).is_ok(),
        "invalid native window: {native:?}"
    );
    let script = guard.dir.join("memory-window.js");
    fs::write(&script, WINDOW_PROBE).unwrap();
    let mut presentation = None;
    wait_for("fully presented memory-gate window", || {
        let census = presentation_census(&script, guard.apps[0].id());
        if preparing && census.interactive && census.frontmost_pid != guard.apps[0].id() {
            return false;
        }
        assert!(
            validate_console(&census, guard.apps[0].id()).is_ok(),
            "memory gate preparation: {:?}",
            validate_console(&census, guard.apps[0].id())
        );
        assert_eq!(
            census.geometry.windows.len(),
            1,
            "requires one visible compositor window"
        );
        let window = census.geometry.windows[0];
        assert_eq!(
            window.scale,
            Some(native.scale),
            "native and measured display scales differ"
        );
        if presentation_matches_native(window, native) {
            presentation = Some(window);
            true
        } else {
            false
        }
    });
    println!("MK066 native window: {native:?}; compositor: {presentation:?}");
    native
}

fn presentation_matches_native(window: MeasuredWindow, native: NativeWindow) -> bool {
    // WindowServer can add a two-point frame. A persistent opening transform
    // (for example on the lock screen) must not count as a rendered viewport.
    window.width.is_finite()
        && window.height.is_finite()
        && (window.width - native.width).abs() <= 2.5
        && (window.height - native.height).abs() <= 2.5
}

fn presentation_census(script: &Path, pid: u32) -> PresentationCensus {
    let output = Command::new("/usr/bin/osascript")
        .args(["-l", "JavaScript"])
        .arg(script)
        .arg(pid.to_string())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "memory gate preparation: window census failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

fn foreground_idle_complete(since: &mut Option<Instant>, focused: bool, now: Instant) -> bool {
    if !focused {
        *since = None;
        return false;
    }
    now.saturating_duration_since(*since.get_or_insert(now)) >= IDLE
}

fn wait_for_idle_console(guard: &Processes) {
    let script = guard.dir.join("memory-window.js");
    let app_pid = guard.apps[0].id();
    let deadline = Instant::now() + WAIT;
    let mut since = None;
    loop {
        let census = presentation_census(&script, app_pid);
        assert!(
            census.interactive,
            "memory gate preparation during idle: requires an unlocked, active macOS console session"
        );
        let now = Instant::now();
        if foreground_idle_complete(&mut since, census.frontmost_pid == app_pid, now) {
            break;
        }
        let remaining = deadline.saturating_duration_since(now);
        assert!(
            !remaining.is_zero(),
            "memory gate preparation: fixture must remain frontmost for {IDLE:?}; expected PID {app_pid}, observed PID {}",
            census.frontmost_pid
        );
        thread::sleep(remaining.min(Duration::from_secs(1)));
    }
}

#[test]
fn memory_window_focus_loss_restarts_the_full_idle_period() {
    let now = Instant::now();
    let mut since = None;
    assert!(!foreground_idle_complete(&mut since, true, now));
    assert!(!foreground_idle_complete(&mut since, true, now + IDLE / 2));
    assert!(!foreground_idle_complete(&mut since, false, now + IDLE));
    assert!(!foreground_idle_complete(&mut since, true, now + IDLE));
    assert!(!foreground_idle_complete(
        &mut since,
        true,
        now + IDLE + IDLE / 2
    ));
    assert!(foreground_idle_complete(&mut since, true, now + IDLE * 2));
}

#[test]
fn memory_window_preconditions_reject_wrong_size_scale_and_window_count() {
    let valid = r#"{"windows":[{"width":1600,"height":1000,"scale":2}]}"#;
    assert!(validate_window(&serde_json::from_str(valid).unwrap()).is_ok());
    for invalid in [
        r#"{"windows":[{"width":1200,"height":760,"scale":2}]}"#,
        r#"{"windows":[{"width":1600,"height":1000,"scale":1}]}"#,
        r#"{"windows":[{"width":1600,"height":1000,"scale":null}]}"#,
        r#"{"windows":[]}"#,
        r#"{"windows":[{"width":1600,"height":1000,"scale":2},{"width":1600,"height":1000,"scale":2}]}"#,
    ] {
        assert!(
            validate_window(&serde_json::from_str(invalid).unwrap()).is_err(),
            "accepted invalid memory measurement setup: {invalid}"
        );
    }
    let native = NativeWindow {
        pid: 1,
        width: 1600.,
        height: 1000.,
        scale: 2.,
        window_count: 1,
        windowed: true,
    };
    for (width, height, valid) in [
        (1600., 1000., true),
        (1602., 1002., true),
        (1440., 900., false),
        (1800., 1200., false),
    ] {
        assert_eq!(
            presentation_matches_native(
                MeasuredWindow {
                    width,
                    height,
                    scale: Some(2.),
                },
                native
            ),
            valid
        );
    }
}

#[test]
fn memory_window_preconditions_reject_locked_or_inactive_fixture() {
    let mut census: PresentationCensus =
        serde_json::from_str(r#"{"interactive":true,"frontmost_pid":42,"windows":[]}"#).unwrap();
    assert!(validate_console(&census, 42).is_ok());
    for other_pid in [0, 41, 43] {
        census.frontmost_pid = other_pid;
        assert!(validate_console(&census, 42).is_err());
    }
    census.frontmost_pid = 42;
    census.interactive = false;
    assert!(validate_console(&census, 42).is_err());
    assert!(
        serde_json::from_str::<PresentationCensus>(r#"{"interactive":true,"windows":[]}"#).is_err()
    );
}

/// `footprint`의 "Footprint: N B". MK-009에서 빈 출력이 나온 적이 있어 세 번까지 다시 잰다.
fn footprint(pid: u32) -> u64 {
    let fp = (0..3).find_map(|_| {
        let out = Command::new("/usr/bin/footprint")
            .args(["-f", "bytes", "-p", &pid.to_string()])
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        let bytes = text
            .split_once("Footprint: ")
            .and_then(|(_, rest)| rest.split_whitespace().next())
            .and_then(|n| n.parse().ok());
        if bytes.is_none() {
            thread::sleep(Duration::from_millis(500));
        }
        bytes
    });
    assert!(fp.is_some(), "footprint failed for pid {pid}");
    fp.unwrap()
}

fn mib(bytes: u64) -> String {
    let kib = u32::try_from(bytes >> 10).unwrap();
    format!("{:.1}", f64::from(kib) / 1024.0)
}

fn measure(guard: &Processes) -> (u64, u64, u64, String) {
    let app = footprint(guard.apps[0].id());
    let krilld = footprint(guard.krilld.unwrap());
    let holder = footprint(guard.holder.id());
    let daemon = krilld + holder;
    let total = app + daemon;
    let mut line = String::new();
    for (name, fp) in [
        ("app", app),
        ("krilld", krilld),
        ("holder", holder),
        ("daemon", daemon),
    ] {
        write!(line, "{name} {} ", mib(fp)).unwrap();
    }
    (
        app,
        daemon,
        total,
        format!("{line}total {} MiB", mib(total)),
    )
}

#[test]
#[ignore = "actual macOS GUI; run via scripts/memory-gate.sh"]
#[expect(
    clippy::assertions_on_constants,
    reason = "debug 빌드 수치를 게이트 결과로 오인하지 않도록 release 프로필을 강제한다"
)]
fn release_ten_terminals_within_200mib() {
    assert!(!cfg!(debug_assertions), "requires the release profile");
    let dir = PathBuf::from(format!("/tmp/mk066-{}", std::process::id()));
    let (paths, mut guard, _) = start_daemon(dir);

    // 생성기는 go 파일을 기다렸다가(앱이 붙은 뒤) 한글·색·120열을 넘는 줄을 내고 done-<i>를 남긴 채 산다.
    let generator = guard.dir.join("generator.pl");
    fs::write(
        &generator,
        format!(
            "$|=1; print \"Krill 임시 메모리 측정: 이 창을 앞에 둔 채 기다려 주세요.\\r\\n\";\n\
             while (!-e \"$ARGV[0]/go\") {{ select undef,undef,undef,0.02; }}\n\
             for $i (1..{GEN_LINES}) {{ printf \"%07d 한글 \\e[3%dm색\\e[0m %s\\r\\n\", $i, $i%8, 'x' x 110; }}\n\
             open $done, '>', \"$ARGV[0]/done-$ARGV[1]\" or die $!; close $done;\n\
             while (1) {{ select undef,undef,undef,1; }}\n"
        ),
    )
    .unwrap();
    let mut layout = Layout::default();
    layout.set_window_bounds(SavedWindowBounds {
        x: 64.,
        y: 64.,
        width: WINDOW_WIDTH,
        height: WINDOW_HEIGHT,
        mode: SavedWindowMode::Windowed,
    });
    for i in 0..TERMINALS {
        let script = format!(
            "stty raw -echo; exec /usr/bin/perl {} {} {i}",
            generator.display(),
            guard.dir.display()
        );
        let Reply::Created { session, pid } = request(
            &paths,
            &Request::Create(Create {
                account: None,
                program: Program::Custom {
                    program: b"/bin/sh".to_vec(),
                    args: vec![b"-c".to_vec(), script.into_bytes()],
                },
                cwd: Vec::new(),
                cols: 120,
                rows: 40,
            }),
        ) else {
            unreachable!("session creation failed");
        };
        guard.sessions.push(pid);
        if i > 0 {
            layout.new_tab();
        }
        layout.set_session(layout.visible()[0], session);
    }
    layout.activate(0);
    layout.set_tab_title(0, Some("임시 메모리 측정"));
    layout.save(&guard.dir.join("layout.bin")).unwrap();

    fs::write(guard.dir.join("memory-window-request"), []).unwrap();
    launch_app(&mut guard);
    let window = assert_window_conditions(&guard, true);
    fs::write(guard.dir.join("go"), []).unwrap();
    wait_for("generator output", || {
        (0..TERMINALS).all(|i| guard.dir.join(format!("done-{i}")).exists())
    });
    // An inactive window can stop presenting output and undercount live drawable storage.
    // Require a fresh full idle period after focus loss, within the existing WAIT bound.
    wait_for_idle_console(&guard);
    assert_eq!(guard.apps[0].try_wait().unwrap(), None, "GUI exited");

    assert_eq!(
        assert_window_conditions(&guard, false),
        window,
        "window changed during workload"
    );

    let (app, daemon, total, line) = measure(&guard);
    assert_eq!(
        assert_window_conditions(&guard, false),
        window,
        "window changed during measurement"
    );
    println!(
        "MK066 {TERMINALS} terminals: {line} (limits: app {} / daemon {} / total {} MiB)",
        mib(APP_LIMIT),
        mib(DAEMON_LIMIT),
        mib(LIMIT)
    );
    assert!(
        app <= APP_LIMIT,
        "app memory gate: {line} (app > {} MiB)",
        mib(APP_LIMIT)
    );
    assert!(
        daemon <= DAEMON_LIMIT,
        "daemon memory gate: {line} (daemon > {} MiB)",
        mib(DAEMON_LIMIT)
    );
    assert!(total <= LIMIT, "memory gate: {line} > {} MiB", mib(LIMIT));
}
