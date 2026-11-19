//! `krill drop` send → ls → rm against an isolated state directory, with no daemon running.
#![expect(clippy::unwrap_used, reason = "integration test")]

use std::fs;
use std::process::{Command, Output};

const MK: &str = env!("CARGO_BIN_EXE_krill");

fn krill(root: &std::path::Path, args: &[&str]) -> Output {
    Command::new(MK)
        .args(args)
        .env("KRILL_STATE_DIR", root.join("state"))
        .env("HOME", root.join("home"))
        .env("USERPROFILE", root.join("home"))
        .env("KRILL_TELEMETRY_DISABLED", "1")
        .env("KRILL_ERROR_REPORTING_DISABLED", "1")
        .output()
        .unwrap()
}

fn json(output: &Output) -> serde_json::Value {
    assert!(output.status.success(), "{output:?}");
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn send_list_remove_round_trip() {
    let root = std::env::temp_dir().join(format!("krill-drop-cli-{}", std::process::id()));
    fs::remove_dir_all(&root).ok();
    fs::create_dir_all(root.join("home")).unwrap();
    let report = root.join("보고서 final.md");
    fs::write(&report, "# result\n").unwrap();
    let report_arg = report.to_str().unwrap();

    let sent = json(&krill(&root, &["drop", report_arg, "--json"]));
    let id = sent["items"][0]["id"].as_str().unwrap().to_owned();
    assert_eq!(sent["items"][0]["name"], "보고서 final.md");
    assert_eq!(sent["items"][0]["size"], 9);

    let listed = json(&krill(&root, &["drop", "ls", "--json"]));
    assert_eq!(listed["items"].as_array().unwrap().len(), 1);
    assert_eq!(listed["items"][0]["id"], id.as_str());
    let inbox = root.join("home").join("Downloads").join("Krill Drop");
    assert_eq!(listed["inbox"], inbox.to_str().unwrap());
    let human = String::from_utf8(krill(&root, &["drop", "ls"]).stdout).unwrap();
    assert!(
        human.contains(&id) && human.contains("보고서 final.md"),
        "{human}"
    );

    let rejected = krill(&root, &["drop", root.to_str().unwrap()]);
    assert!(!rejected.status.success());
    assert!(
        !krill(&root, &["drop", "rm", "../../state"])
            .status
            .success()
    );

    assert!(krill(&root, &["drop", "rm", &id]).status.success());
    assert!(krill(&root, &["drop", "rm", &id]).status.success());
    assert_eq!(
        json(&krill(&root, &["drop", "ls", "--json"]))["items"],
        serde_json::json!([])
    );
    assert!(report.exists());
    fs::remove_dir_all(&root).unwrap();
}
