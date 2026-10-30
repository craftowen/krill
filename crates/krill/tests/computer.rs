//! Computer executor transport regressions; never drives the physical desktop.
#![cfg(target_os = "macos")]
#![expect(
    clippy::unwrap_used,
    clippy::panic,
    reason = "integration-test fixture and diagnostic helpers"
)]

use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;
use std::process::{Command, Output, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_krill");

#[test]
fn production_jxa_contract_under_synthetic_bridge() {
    // Execute the exact production bytes in JavaScriptCore, with fake AX/input/clipboard
    // objects. This exercises the interpreter and action branches without touching the GUI.
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let output = Command::new("/usr/bin/osascript")
        .args(["-l", "JavaScript"])
        .arg(root.join("tests/fixtures/computer_jxa.js"))
        .arg(root.join("src/computer/engine/scripts"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["failed"], 0, "{report}");
    assert_eq!(report["total"], 50);
}

fn fixture_executor(dir: &Path) -> std::path::PathBuf {
    let path = dir.join("fake-osascript.py");
    fs::write(&path, r"#!/usr/bin/env python3
import json, os, pathlib, stat, sys
base=pathlib.Path(os.environ['KRILL_STATE_DIR'])
with (base/'argv.jsonl').open('a') as f: f.write(json.dumps(sys.argv[1:])+'\n')
script=sys.argv[4]
# Real osascript getopt-parses every dash-leading arg after -e unless it follows a lone `--`.
rest=sys.argv[5:]
if rest and rest[0]!='--' and any(a.startswith('-') for a in rest):
    bad=next(a for a in rest if a.startswith('-'))
    print('/usr/bin/osascript: illegal option -- '+bad[1:2],file=sys.stderr); sys.exit(2)
root={'role':'AXWindow','title':'Fixture','bounds':{'x':0,'y':0,'width':400,'height':300},'actions':[],
      'children':[{'role':'AXTextField','title':'Input','value':'before','focused':True,'enabled':True,
                   'bounds':{'x':10,'y':20,'width':200,'height':30},'actions':[],'children':[]}]}
if '--request-file' in sys.argv:
    path=pathlib.Path(sys.argv[sys.argv.index('--request-file')+1])
    assert stat.S_IMODE(path.stat().st_mode)==0o600
    request=json.loads(path.read_text())
    (base/'observed-payload.json').write_text(json.dumps(request))
    with (base/'effects').open('a') as f: f.write(request['kind']+'\n')
    mode=(base/'mode').read_text()
    if mode=='unknown':
        print(json.dumps({'ok':True,'status':'verified','delivery':'unknown','cleanup':'not_needed'}))
    elif mode=='malformed': print('response lost after dispatch')
    else: print(json.dumps({'ok':True,'status':'unverified','delivery':'submitted','cleanup':'not_needed'}))
elif 'const rawApps' in script:
    print(json.dumps([{'name':'Fixture','bundleId':'dev.fixture','pid':7777,'isActive':True,'isHidden':False}]))
elif 'let idx = 1;' in script:
    print(json.dumps([{'id':'101','index':1,'title':'Fixture','appName':'Fixture','bundleId':'dev.fixture','pid':7777,
                      'bounds':{'x':0,'y':0,'width':400,'height':300},'isFocused':True,'isMain':True,'isMinimized':False}]))
elif 'const start = processStart()' in script:
    if (base/'mode').read_text()=='observation-fails' and (base/'effects').exists():
        print('AX observation unavailable after issued input',file=sys.stderr); sys.exit(75)
    print(json.dumps({'root':root,'processStart':'1000.123456789','truncated':False}))
else:
    print(json.dumps({'accessibility':True,'screenshots':True}))
").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
    path
}

fn command(dir: &Path, executor: &Path, args: &[&str]) -> Command {
    let mut command = Command::new(BIN);
    command
        .arg("computer")
        .args(args)
        .env("KRILL_STATE_DIR", dir)
        .env("KRILL_TEST_OFFLINE", "1")
        .env("KRILL_COMPUTER_OSASCRIPT_BIN", executor);
    command
}

fn snapshot(dir: &Path, executor: &Path) -> serde_json::Value {
    let out = command(
        dir,
        executor,
        &["get-app-state", "--app", "pid:7777", "--json"],
    )
    .output()
    .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap()
}

fn result(out: &Output) -> serde_json::Value {
    serde_json::from_slice(&out.stdout).unwrap_or_else(|error| {
        panic!(
            "result {error}: stdout={} stderr={}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )
    })
}

#[test]
fn stdin_secret_never_enters_executor_argv_and_unknown_never_replays() {
    use std::io::Write as _;
    let dir = std::env::temp_dir().join(format!("krill-computer-private-{}", std::process::id()));
    fs::remove_dir_all(&dir).ok();
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("mode"), "unknown").unwrap();
    let executor = fixture_executor(&dir);
    let observation = snapshot(&dir, &executor);
    assert!(
        observation.get("root").is_none(),
        "default output should be compact"
    );
    let secret = "fixture phrase";
    let mut child = command(
        &dir,
        &executor,
        &[
            "set-value",
            "--snapshot",
            observation["snapshotId"].as_str().unwrap(),
            "--element",
            "1",
            "--value-stdin",
            "--json",
        ],
    )
    .stdin(Stdio::piped())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .spawn()
    .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(secret.as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(!output.status.success());
    let payload = result(&output);
    assert_eq!(payload["delivery"], "unknown");
    assert_eq!(payload["verification"]["status"], "unverified");
    assert_eq!(
        fs::read_to_string(dir.join("effects"))
            .unwrap()
            .lines()
            .count(),
        1
    );
    let argv = fs::read_to_string(dir.join("argv.jsonl")).unwrap();
    assert!(!argv.contains(secret));
    let observed: serde_json::Value =
        serde_json::from_slice(&fs::read(dir.join("observed-payload.json")).unwrap()).unwrap();
    assert_eq!(observed["value"], secret);
    let action_args: Vec<String> = argv
        .lines()
        .map(|line| serde_json::from_str::<Vec<String>>(line).unwrap())
        .find(|args| args.iter().any(|arg| arg == "--request-file"))
        .unwrap();
    let file_index = action_args
        .iter()
        .position(|arg| arg == "--request-file")
        .unwrap()
        + 1;
    assert!(
        !Path::new(&action_args[file_index]).exists(),
        "reaped child payload must be removed"
    );
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn observation_failure_preserves_submitted_delivery_and_malformed_response_is_unknown() {
    for mode in ["observation-fails", "malformed"] {
        let dir =
            std::env::temp_dir().join(format!("krill-computer-{mode}-{}", std::process::id()));
        fs::remove_dir_all(&dir).ok();
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("mode"), mode).unwrap();
        let executor = fixture_executor(&dir);
        snapshot(&dir, &executor);
        let output = command(&dir, &executor, &["click", "--element", "1", "--json"])
            .output()
            .unwrap();
        assert!(!output.status.success());
        let payload = result(&output);
        assert_eq!(payload["verification"]["status"], "unverified");
        if mode == "observation-fails" {
            assert_eq!(payload["delivery"], "submitted");
            assert_eq!(payload["observation"]["status"], "failed");
            assert!(payload.get("appState").is_none());
        } else {
            assert_eq!(payload["delivery"], "unknown");
        }
        assert_eq!(
            fs::read_to_string(dir.join("effects"))
                .unwrap()
                .lines()
                .count(),
            1
        );
        fs::remove_dir_all(&dir).ok();
    }
}

#[test]
fn snapshot_cache_is_atomic_immutable_and_parent_scoped() {
    let dir = std::env::temp_dir().join(format!("krill-computer-cache-{}", std::process::id()));
    fs::remove_dir_all(&dir).ok();
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("mode"), "submitted").unwrap();
    let executor = fixture_executor(&dir);
    let observations = std::thread::scope(|scope| {
        let first = scope.spawn(|| snapshot(&dir, &executor));
        let second = scope.spawn(|| snapshot(&dir, &executor));
        [first.join().unwrap(), second.join().unwrap()]
    });
    assert_ne!(observations[0]["snapshotId"], observations[1]["snapshotId"]);
    let cache_dir = fs::read_dir(dir.join("computer-snapshots"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let original_path = cache_dir.join(format!(
        "{}.json",
        observations[0]["snapshotId"].as_str().unwrap()
    ));
    let original_bytes = fs::read(&original_path).unwrap();
    let original: serde_json::Value = serde_json::from_slice(&original_bytes).unwrap();
    assert!(
        original.get("root").is_some(),
        "private cache retains full identity witness"
    );
    let latest: serde_json::Value =
        serde_json::from_slice(&fs::read(cache_dir.join("computer-state.json")).unwrap()).unwrap();
    assert!(
        observations
            .iter()
            .any(|item| item["snapshotId"] == latest["snapshotId"])
    );
    let page = command(
        &dir,
        &executor,
        &[
            "get-app-state",
            "--snapshot",
            observations[0]["snapshotId"].as_str().unwrap(),
            "--offset",
            "0",
            "--limit",
            "1",
            "--json",
        ],
    )
    .output()
    .unwrap();
    assert!(page.status.success());
    assert_eq!(result(&page)["snapshotId"], observations[0]["snapshotId"]);
    assert_eq!(fs::read(&original_path).unwrap(), original_bytes);

    // A real intermediate parent is not the original calling context. Claimed session/env IDs do not help.
    let mut other = Command::new("/bin/sh");
    let output = other.arg("-c").arg("\"$1\" computer click --element 1 --session claimed-session --json; result=$?; exit \"$result\"")
        .arg("caller-isolation").arg(BIN).env("KRILL_STATE_DIR", &dir).env("KRILL_TEST_OFFLINE", "1")
        .env("KRILL_COMPUTER_OSASCRIPT_BIN", &executor).env("MK_PANE_ID", "claimed-session").output().unwrap();
    assert!(!output.status.success());
    assert!(!dir.join("effects").exists());
    fs::remove_dir_all(&dir).ok();
}
