//! MK-048 real CLI tests. Every child gets temporary HOME/state/config roots; no real user settings.
#![cfg_attr(unix, expect(clippy::unwrap_used, reason = "integration test helpers"))]
#![cfg(unix)]

use std::fs;
use std::io::{Read as _, Write as _};
use std::os::unix::fs::{PermissionsExt as _, symlink};
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

use krill_proto::agent::hooks::{self, Agent};
use serde_json::{Value, json};

const MK: &str = env!("CARGO_BIN_EXE_krill");
static SERIAL: AtomicU64 = AtomicU64::new(0);

struct Home(PathBuf);
impl Home {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!(
            "mk413-{}-{}",
            std::process::id(),
            SERIAL.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();
        Self(fs::canonicalize(dir).unwrap())
    }
    fn config(&self, agent: &str) -> PathBuf {
        self.0.join(match agent {
            "claude" => ".claude/settings.json",
            "cursor" => ".cursor/hooks.json",
            "agy" | "antigravity" => ".gemini/config/hooks.json",
            "opencode" => ".config/opencode/plugins/minikrill.js",
            _ => ".codex/config.toml",
        })
    }
    fn child(&self) -> Command {
        let mut c = Command::new(MK);
        c.env("HOME", &self.0)
            .env("KRILL_STATE_DIR", self.0.join("state"))
            .env_remove("CODEX_HOME")
            .env_remove("CLAUDE_CONFIG_DIR")
            .env_remove("OPENCODE_CONFIG_DIR")
            .env_remove("XDG_CONFIG_HOME")
            .env_remove("MK_PANE_ID");
        c
    }
    fn run(&self, action: &str, agent: &str, flags: &[&str], answer: &str) -> Output {
        let mut c = self.child();
        c.args(["hook", action, agent]).args(flags);
        output(&mut c, answer)
    }
    fn write(&self, agent: &str, text: &str) {
        let path = self.config(agent);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }
    fn read(&self, agent: &str) -> String {
        fs::read_to_string(self.config(agent)).unwrap()
    }
    fn value(&self, agent: &str) -> Value {
        serde_json::from_str(&self.read(agent)).unwrap()
    }
    fn fake_codex(&self) -> PathBuf {
        let bin = self.0.join("bin");
        fs::create_dir_all(&bin).unwrap();
        let script = bin.join("codex");
        fs::write(&script, include_str!("fixtures/mk413-codex.py")).unwrap();
        fs::set_permissions(script, fs::Permissions::from_mode(0o700)).unwrap();
        bin
    }
    fn codex(&self, action: &str, flags: &[&str]) -> Output {
        let mut c = self.child();
        c.env("PATH", self.fake_codex())
            .args(["hook", action, "codex"])
            .args(flags);
        output(&mut c, "n\n")
    }
}
impl Drop for Home {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).ok();
    }
}

fn output(c: &mut Command, input: &str) -> Output {
    let mut child = c
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(input.as_bytes()).ok();
    child.wait_with_output().unwrap()
}

fn success(o: &Output) {
    assert!(
        o.status.success(),
        "stdout: {} stderr: {}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
}

#[test]
fn preview_and_refusal_do_not_write_any_files() {
    for agent in ["cursor", "opencode"] {
        let h = Home::new();
        let preview = h.run("install", agent, &["--dry-run"], "");
        success(&preview);
        assert!(String::from_utf8_lossy(&preview.stdout).contains("+++"));
        success(&h.run("install", agent, &[], "no\n"));
        assert!(!h.config(agent).exists());
        assert!(!h.0.join("state").exists());
    }
}

#[test]
fn claude_install_uninstall_preserves_settings_and_user_hooks() {
    let h = Home::new();
    let original = json!({"theme":"dark", "hooks":{"Stop":[{"hooks":[{"type":"command", "command":"user-hook"}]}]}}).to_string();
    h.write("claude", &original);
    success(&h.run("install", "claude", &["--dry-run"], ""));
    assert_eq!(h.read("claude"), original);
    success(&h.run("install", "claude", &["--yes"], ""));
    let installed = h.value("claude");
    assert_eq!(installed["theme"], "dark");
    assert!(installed.to_string().contains("MK_PANE_ID"));
    assert!(installed.to_string().contains("hook claude"));
    assert!(installed.to_string().contains("user-hook"));
    success(&h.run("uninstall", "claude", &["--yes"], ""));
    assert_eq!(h.read("claude"), original);
}

#[test]
fn shared_api_and_cli_exchange_receipts_and_use_the_same_install_lock() {
    use krill_proto::hook_install::{Action, Plan, TargetAgent, install_hooks, store};
    use std::path::Path;

    let h = Home::new();
    let original = "{\n\t\"user\":\"한글\",\"hooks\":{}\n}\n";
    h.write("claude", original);
    let state = h.0.join("state");
    let lock = store::lock(&state.join("hooks")).unwrap();
    let blocked = h.run("install", "claude", &["--yes"], "");
    assert!(!blocked.status.success());
    assert!(String::from_utf8_lossy(&blocked.stderr).contains("another hook operation"));
    assert_eq!(h.read("claude"), original);
    assert!(!state.join("hooks/claude.json").exists());
    drop(lock);

    let receipt = install_hooks(
        TargetAgent::Claude,
        &h.config("claude"),
        &state,
        Path::new(MK),
    )
    .unwrap()
    .unwrap();
    assert!(receipt.complete);
    success(&h.run("uninstall", "claude", &["--yes"], ""));
    assert_eq!(h.read("claude"), original);
    assert!(!state.join("hooks/claude.json").exists());

    success(&h.run("install", "claude", &["--yes"], ""));
    Plan::prepare(
        Action::Uninstall,
        TargetAgent::Claude,
        &h.config("claude"),
        &state,
        Path::new(MK),
    )
    .unwrap()
    .unwrap()
    .apply()
    .unwrap();
    assert_eq!(h.read("claude"), original);
    assert!(!state.join("hooks/claude.json").exists());
}

#[test]
fn schedule_and_hook_usage_errors_exit_two_without_writes() {
    let h = Home::new();
    for args in [
        vec!["schedule"],
        vec!["schedule", "unknown"],
        vec!["schedule", "add", "missing"],
        vec![
            "schedule",
            "add",
            "bad-cron",
            "--cron",
            "invalid",
            "--prompt",
            "hello",
            "--",
            "/bin/true",
        ],
        vec!["hook", "install"],
        vec!["hook", "install", "unknown"],
        vec!["hook", "install", "claude", "--unknown"],
    ] {
        let out = h.child().args(&args).output().unwrap();
        assert_eq!(out.status.code(), Some(2), "{args:?}: {out:?}");
    }
    assert!(!h.0.join("state/schedules.toml").exists());
    assert!(!h.config("claude").exists());
}

#[test]
fn unmodified_settings_roundtrip_bytes_and_keep_private_backups() {
    for agent in ["cursor"] {
        let h = Home::new();
        let original = "{\n\t\"user\": \"한글\", \"hooks\": {}\n}\n";
        h.write(agent, original);
        fs::set_permissions(h.config(agent), fs::Permissions::from_mode(0o640)).unwrap();
        success(&h.run("install", agent, &["--yes"], ""));
        assert_eq!(
            fs::metadata(h.config(agent)).unwrap().permissions().mode() & 0o777,
            0o640
        );
        success(&h.run("uninstall", agent, &["--yes"], ""));
        assert_eq!(h.read(agent), original);
        let backups: Vec<_> = fs::read_dir(h.config(agent).parent().unwrap())
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().contains("minikrill-bak"))
            .collect();
        assert_eq!(backups.len(), 2);
        for b in backups {
            assert_eq!(b.metadata().unwrap().permissions().mode() & 0o777, 0o600);
        }
        success(&h.run("uninstall", agent, &["--yes"], ""));
    }
}

#[test]
fn remove_only_owned_entries_after_user_edits_and_reordering() {
    let (agent, event, entry) = ("cursor", "stop", json!({"command":"user before"}));
    let h = Home::new();
    h.write(agent, &json!({"hooks":{event:[entry]}}).to_string());
    success(&h.run("install", agent, &["--yes"], ""));
    let installed = h.read(agent);
    success(&h.run("install", agent, &["--yes"], ""));
    assert_eq!(h.read(agent), installed);
    let mut value = h.value(agent);
    value["newUserSetting"] = json!({"theme":"dark"});
    let entries = value["hooks"][event].as_array_mut().unwrap();
    entries[0] = json!({"command":"user edited"});
    entries.reverse();
    entries.push(json!({"command":"new unrelated hook"}));
    h.write(agent, &value.to_string());
    success(&h.run("uninstall", agent, &["--yes"], ""));
    let value = h.value(agent);
    assert_eq!(value["newUserSetting"], json!({"theme":"dark"}));
    assert_eq!(value["hooks"][event].as_array().unwrap().len(), 2);
    assert!(value.to_string().contains("user edited"));
    assert!(!value.to_string().contains("MK_PANE_ID"));
}

#[test]
fn a_user_modified_managed_entry_is_preserved() {
    let h = Home::new();
    success(&h.run("install", "cursor", &["--yes"], ""));
    let mut value = h.value("cursor");
    value["hooks"]["stop"][0]["command"] = json!("user replacement");
    value["version"] = json!(2);
    h.write("cursor", &value.to_string());
    success(&h.run("uninstall", "cursor", &["--yes"], ""));
    assert_eq!(
        h.value("cursor"),
        json!({"version":2,"hooks":{"stop":[{"command":"user replacement"}]}})
    );
}

#[test]
fn preexisting_identical_entries_remain_user_owned() {
    let h = Home::new();
    let cmd = hooks::command(std::path::Path::new(MK), Agent::Cursor).unwrap();
    let original = json!({"version":1,"hooks":{"stop":[{"command":cmd}]}}).to_string();
    h.write("cursor", &original);
    success(&h.run("install", "cursor", &["--yes"], ""));
    success(&h.run("uninstall", "cursor", &["--yes"], ""));
    assert_eq!(h.read("cursor"), original);
}

#[test]
fn opencode_only_owns_its_plugin_and_respects_config_override() {
    let h = Home::new();
    success(&h.run("install", "opencode", &["--yes"], ""));
    assert!(
        h.read("opencode")
            .contains("if (!process.env.MK_PANE_ID) return {}")
    );
    success(&h.run("uninstall", "opencode", &["--yes"], ""));
    assert!(!h.config("opencode").exists());
    h.write("opencode", "// user's plugin");
    assert!(
        !h.run("install", "opencode", &["--yes"], "")
            .status
            .success()
    );
    fs::remove_file(h.config("opencode")).unwrap();
    success(&h.run("install", "opencode", &["--yes"], ""));
    h.write("opencode", "// user's edited plugin");
    success(&h.run("uninstall", "opencode", &["--yes"], ""));
    assert_eq!(h.read("opencode"), "// user's edited plugin");
    let custom = h.0.join("custom-opencode");
    let mut c = h.child();
    c.env("OPENCODE_CONFIG_DIR", &custom)
        .args(["hook", "install", "opencode", "--yes"]);
    success(&output(&mut c, ""));
    assert!(custom.join("plugins/minikrill.js").exists());
    success(&h.run("uninstall", "opencode", &["--yes"], ""));
    assert!(!custom.join("plugins/minikrill.js").exists());
}

#[test]
fn malformed_settings_and_symlinks_are_never_replaced() {
    let h = Home::new();
    for text in [
        "{bad",
        "[]",
        "{\"hooks\":false}",
        "{\"hooks\":{\"stop\":{}}}",
        "{\"version\":2}",
    ] {
        h.write("cursor", text);
        assert!(!h.run("install", "cursor", &["--yes"], "").status.success());
        assert_eq!(h.read("cursor"), text);
        assert!(!h.0.join("state").exists());
    }
    fs::remove_file(h.config("cursor")).unwrap();
    let target = h.0.join("user.json");
    fs::write(&target, "{}").unwrap();
    symlink(&target, h.config("cursor")).unwrap();
    assert!(!h.run("install", "cursor", &["--yes"], "").status.success());
    assert_eq!(fs::read_to_string(target).unwrap(), "{}");
}

#[test]
fn installed_shell_guard_is_noop_without_pane_and_safe_with_special_paths() {
    let h = Home::new();
    let exe = h.0.join("krill 한글 ' $` command");
    fs::copy(MK, &exe).unwrap();
    let command = hooks::command(&exe, Agent::Agy).unwrap();
    let mut outside = Command::new("/bin/sh");
    outside
        .args(["-c", &command])
        .env_remove("MK_PANE_ID")
        .env("HOME", &h.0)
        .env("KRILL_STATE_DIR", h.0.join("state"));
    let o = output(&mut outside, "{}");
    success(&o);
    assert_eq!(o.stdout, b"{}\n");
    assert!(!h.0.join("state").exists());
    outside.env("MK_PANE_ID", "test-pane");
    success(&output(
        &mut outside,
        "{\"hook_event_name\":\"PostInvocation\"}",
    ));
    let spool = fs::read(h.0.join("state/hook.spool")).unwrap();
    assert_eq!(
        krill_proto::hook::read_spool(&spool).first().unwrap().agent,
        "agy"
    );
}

#[test]
fn codex_rpc_install_preview_remove_preserves_user_trust_and_config() {
    let h = Home::new();
    h.write("codex", "# user comment\nmodel = 'user-model'\n[hooks.state.\"user-key\"]\ntrusted_hash = 'user-hash'\n");
    let original = h.read("codex");
    success(&h.codex("install", &["--dry-run"]));
    assert_eq!(h.read("codex"), original);
    assert!(!h.0.join("state").exists());
    success(&h.codex("install", &[]));
    assert_eq!(h.read("codex"), original);
    success(&h.codex("install", &["--yes"]));
    let installed: toml::Value = toml::from_str(&h.read("codex")).unwrap();
    assert_eq!(
        installed["hooks"]["state"].as_table().unwrap().len(),
        Agent::Codex.events().len() + 1
    );
    assert!(!h.0.join(".codex/hooks.json").exists());
    assert!(!h.0.join(".codex/auth.json").exists());
    h.write(
        "codex",
        &(h.read("codex") + "\n[hooks.state.\"later-user\"]\ntrusted_hash = 'later-hash'\n"),
    );
    success(&h.codex("uninstall", &["--yes"]));
    let removed: toml::Value = toml::from_str(&h.read("codex")).unwrap();
    assert_eq!(removed["model"].as_str(), Some("user-model"));
    assert_eq!(removed["hooks"]["state"].as_table().unwrap().len(), 2);
    assert!(h.read("codex").starts_with("# user comment"));
}

/// Finder로 뜬 앱은 launchd PATH라 `codex`를 못 찾는다. 호출자가 준 PATH로 찾아야 한다.
#[test]
fn codex_rpc_uses_the_caller_supplied_search_path() {
    use krill_proto::hook_install::{Action, Plan, TargetAgent};
    use std::path::Path;
    let h = Home::new();
    h.write("codex", "# original\n");
    let bin = h.fake_codex();
    let receipt = Plan::prepare_with_path(
        Action::Install,
        TargetAgent::Other(Agent::Codex),
        &h.config("codex"),
        &h.0.join("state"),
        Path::new(MK),
        Some(bin.as_os_str()),
    )
    .unwrap()
    .unwrap()
    .apply()
    .unwrap();
    assert!(receipt.complete);
    let installed: toml::Value = toml::from_str(&h.read("codex")).unwrap();
    assert!(!installed["hooks"]["state"].as_table().unwrap().is_empty());
}

#[test]
fn codex_trust_conflicts_are_rejected_and_user_modified_hash_is_preserved() {
    let h = Home::new();
    let key = "/<session-flags>/config.toml:stop:0:0";
    h.write(
        "codex",
        &format!("[hooks.state.\"{key}\"]\ntrusted_hash = 'other-owner'\n"),
    );
    assert!(!h.codex("install", &["--yes"]).status.success());
    assert!(h.read("codex").contains("other-owner"));
    assert!(!h.0.join("state").exists());
    h.write("codex", "# original\n");
    success(&h.codex("install", &["--yes"]));
    let mut text = h.read("codex");
    let state: toml::Value = toml::from_str(&text).unwrap();
    let hash = state["hooks"]["state"][key]["trusted_hash"]
        .as_str()
        .unwrap();
    text = text.replace(hash, "user-new-hash");
    h.write("codex", &text);
    success(&h.codex("uninstall", &["--yes"]));
    let state: toml::Value = toml::from_str(&h.read("codex")).unwrap();
    assert_eq!(state["hooks"]["state"].as_table().unwrap().len(), 1);
    assert_eq!(
        state["hooks"]["state"][key]["trusted_hash"].as_str(),
        Some("user-new-hash")
    );
}

#[test]
fn configuration_changed_while_consent_is_pending_is_not_overwritten() {
    let h = Home::new();
    h.write("cursor", "{}");
    let mut child = h
        .child()
        .args(["hook", "install", "cursor"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdout = child.stdout.take().unwrap();
    let mut seen = String::new();
    loop {
        let mut byte = [0];
        stdout.read_exact(&mut byte).unwrap();
        seen.push(char::from(byte[0]));
        if seen.ends_with("[y/N] ") {
            break;
        }
    }
    h.write("cursor", "{\"user\":\"saved while reviewing\"}");
    child.stdin.take().unwrap().write_all(b"yes\n").unwrap();
    assert!(!child.wait().unwrap().success());
    assert_eq!(h.read("cursor"), "{\"user\":\"saved while reviewing\"}");
    assert!(!h.0.join("state/hooks/cursor.json").exists());
}

#[test]
fn failed_codex_write_keeps_a_recoverable_receipt_and_does_not_claim_success_on_retry() {
    let h = Home::new();
    h.write("codex", "model = 'user-model'\n");
    let original = h.read("codex");
    let mut c = h.child();
    c.env("PATH", h.fake_codex())
        .env("MK413_FAKE_FAIL_WRITE", "1")
        .args(["hook", "install", "codex", "--yes"]);
    assert!(!output(&mut c, "").status.success());
    assert_eq!(h.read("codex"), original);
    assert!(h.0.join("state/hooks/codex.json").exists());
    assert!(!h.codex("install", &["--yes"]).status.success());
    success(&h.codex("uninstall", &["--yes"]));
    assert_eq!(h.read("codex"), original);
    assert!(!h.0.join("state/hooks/codex.json").exists());
}

#[test]
fn opencode_plugin_executes_under_bun_and_does_not_spawn_outside_minikrill() {
    let Some(bun) = krill_proto::agent::which("bun", &std::env::var_os("PATH").unwrap_or_default())
    else {
        return;
    };
    let h = Home::new();
    success(&h.run("install", "opencode", &["--yes"], ""));
    let script = h.0.join("probe.mjs");
    fs::write(&script, r"
import { KrillHook } from './.config/opencode/plugins/minikrill.js';
let calls = 0;
Bun.spawn = (args, options) => {
  calls++;
  const payload = JSON.parse(new TextDecoder().decode(options.stdin));
  if (args[1] !== 'hook' || args[2] !== 'opencode' || payload.hook_event_name !== 'Stop' || payload.session_id !== 's1') throw Error('bad payload');
  return { exited: Promise.resolve(0) };
};
const callbacks = await KrillHook();
if (!process.env.MK_PANE_ID) {
  if (Object.keys(callbacks).length || calls) throw Error('outside pane spawned a hook');
} else {
  await callbacks.event({ event: { type: 'session.idle', properties: { sessionID: 's1' } } });
  if (calls !== 1) throw Error('missing hook');
}
").unwrap();
    let mut c = Command::new(bun);
    c.arg(script).env("HOME", &h.0).env_remove("MK_PANE_ID");
    success(&output(&mut c, ""));
    c.env("MK_PANE_ID", "pane");
    success(&output(&mut c, ""));
}

#[test]
fn agy_hook_install_roundtrip_and_pre_tool_use_response() {
    let h = Home::new();
    let original = json!({
        "orca-status": {
            "PreInvocation": [{"type": "command", "command": "true"}]
        }
    })
    .to_string();
    h.write("agy", &original);
    success(&h.run("install", "agy", &["--yes"], ""));
    let installed = h.value("agy");
    assert!(installed.get("orca-status").is_some());
    assert!(installed["minikrill"]["PreInvocation"].is_array());
    assert!(installed["minikrill"]["PreToolUse"][0]["hooks"].is_array());

    // Execute the installed PreToolUse command inside a pane and verify it returns `{"decision":"ask"}`
    let pre_cmd = installed["minikrill"]["PreToolUse"][0]["hooks"][0]["command"]
        .as_str()
        .unwrap();
    let mut cmd = Command::new("/bin/sh");
    cmd.args(["-c", pre_cmd])
        .env("MK_PANE_ID", "test-pane")
        .env("HOME", &h.0)
        .env("KRILL_STATE_DIR", h.0.join("state"));
    let o = output(&mut cmd, r#"{"tool_name":"ask_question"}"#);
    success(&o);
    assert_eq!(o.stdout, b"{\"decision\":\"ask\"}\n");
    let spool = fs::read(h.0.join("state/hook.spool")).unwrap();
    let events = krill_proto::hook::read_spool(&spool);
    assert_eq!(events.first().unwrap().agent, "agy");
    let payload: Value = serde_json::from_slice(&events.first().unwrap().payload).unwrap();
    assert_eq!(payload["hook_event_name"], "PreToolUse");
    assert_eq!(payload["tool_name"], "ask_question");

    // Uninstall removes only minikrill bundle and restores original
    success(&h.run("uninstall", "agy", &["--yes"], ""));
    assert_eq!(h.read("agy"), original);
}
