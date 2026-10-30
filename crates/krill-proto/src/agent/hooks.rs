//! MK-048 hook definitions. Sources: <https://learn.chatgpt.com/docs/hooks>,
//! <https://cursor.com/docs/hooks>,
//! <https://opencode.ai/docs/plugins/>. Codex uses MK-012 session flags, never an auth overlay.

use std::io;
use std::path::Path;

use serde_json::json;

/// Supported persistent hook integrations. Claude uses per-session settings (MK-040).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Agent {
    Codex,
    OpenCode,
    Cursor,
    Agy,
    Pi,
}

impl Agent {
    pub fn parse(id: &str) -> Option<Self> {
        match id {
            "codex" => Some(Self::Codex),
            "opencode" => Some(Self::OpenCode),
            "cursor" => Some(Self::Cursor),
            "pi" => Some(Self::Pi),
            "agy" | "antigravity" => Some(Self::Agy),
            _ => None,
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::OpenCode => "opencode",
            Self::Cursor => "cursor",
            Self::Agy => "agy",
            Self::Pi => "pi",
        }
    }

    pub fn events(self) -> &'static [&'static str] {
        match self {
            Self::Codex => &[
                "SessionStart",
                "UserPromptSubmit",
                "PreToolUse",
                "PostToolUse",
                "PermissionRequest",
                "Stop",
                "Interrupt",
                "SessionEnd",
            ],
            // Observational events only: never return an allow/deny decision to Cursor.
            Self::Cursor => &[
                "sessionStart",
                "beforeSubmitPrompt",
                "afterShellExecution",
                "afterMCPExecution",
                "stop",
                "sessionEnd",
            ],
            Self::Agy => &[
                "PreInvocation",
                "PostInvocation",
                "Stop",
                "PreToolUse",
                "PostToolUse",
            ],
            Self::OpenCode | Self::Pi => &[],
        }
    }
}

/// `krill hook install`의 설치 receipt. krilld가 Codex 세션 인자를 이 receipt의 고정 명령으로 만든다.
pub fn receipt(state: &Path, agent: Agent) -> std::path::PathBuf {
    state.join("hooks").join(format!("{}.json", agent.id()))
}

/// POSIX single-quote escaping. No command substitution, even for unusual install paths.
pub fn shell_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\"'\"'"))
}

/// Fixed command: pane identity stays in the environment so Codex trust hashes stay stable.
pub fn command(exe: &Path, agent: Agent) -> Option<String> {
    let exe = exe.to_str()?;
    #[cfg(windows)]
    if agent == Agent::Codex {
        return Some(windows_codex_command(exe));
    }
    Some(format!(
        "if [ -n \"${{MK_PANE_ID:-}}\" ]; then exec {} hook {}; else printf '{{}}\\n'; fi",
        shell_quote(exe),
        agent.id()
    ))
}

#[cfg(any(windows, test))]
fn windows_codex_command(exe: &str) -> String {
    use base64::Engine as _;
    // Codex starts this through cmd.exe. Encoding protects the literal path;
    // ProcessStartInfo forwards raw stdin without PowerShell's object pipeline.
    let script = format!(
        "$ErrorActionPreference='Stop'; \
         if (-not $env:MK_PANE_ID) {{ [Console]::WriteLine('{{}}'); exit 0 }}; \
         $info=[Diagnostics.ProcessStartInfo]::new(); \
         $info.FileName='{}'; $info.Arguments='hook codex'; \
         $info.UseShellExecute=$false; $info.RedirectStandardInput=$true; \
         $info.CreateNoWindow=$true; $child=[Diagnostics.Process]::Start($info); \
         try {{ [Console]::OpenStandardInput().CopyTo($child.StandardInput.BaseStream) }} \
         finally {{ $child.StandardInput.Close() }}; \
         $child.WaitForExit(); $code=$child.ExitCode; $child.Dispose(); exit $code",
        exe.replace('\'', "''")
    );
    let bytes: Vec<_> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
    let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
    format!("powershell.exe -NoLogo -NoProfile -NonInteractive -EncodedCommand {encoded}")
}

#[cfg(all(windows, test))]
mod windows_tests;

/// Event from Codex's synthetic session layer key. Keep the original key when
/// persisting trust: Windows uses C:\\<session-flags> while Unix uses /<session-flags>.
pub fn codex_session_event(key: &str) -> Option<&str> {
    let tail = key
        .strip_prefix("/<session-flags>/config.toml:")
        .or_else(|| key.strip_prefix(r"C:\<session-flags>\config.toml:"))
        .or_else(|| key.strip_prefix("C:/<session-flags>/config.toml:"))?;
    let (event, slot) = tail.split_once(':')?;
    (!event.is_empty() && slot == "0:0").then_some(event)
}

/// Wrap an Antigravity (`agy`) hook command in `/bin/sh -c` with event propagation and
/// non-denying fallback output (`{"decision":"ask"}` on `PreToolUse`).
#[must_use]
pub fn agy_event_command(command: &str, event: &str) -> String {
    let fallback = if event == "PreToolUse" {
        r#"printf '{"decision":"ask"}\n'"#
    } else if event == "Stop" {
        r#"printf '{"decision":""}\n'"#
    } else {
        r"printf '{}\n'"
    };
    let patched = command
        .replace("hook agy", &format!("hook agy {}", shell_quote(event)))
        .replace("printf '{}\\n'", fallback);
    let script = format!("export MK_HOOK_EVENT={}; {patched}", shell_quote(event));
    format!("/bin/sh -c {}", shell_quote(&script))
}

/// Session-only overrides. Pass these to both app-server trust discovery and the TUI launcher.
/// A JSON string is also a valid TOML basic string (commands contain no control characters).
pub fn codex_args(command: &str) -> Vec<String> {
    let quoted = serde_json::Value::String(command.to_owned()).to_string();
    Agent::Codex
        .events()
        .iter()
        .flat_map(|event| {
            [
                "-c".to_owned(),
                format!("hooks.{event}=[{{hooks=[{{type=\"command\",command={quoted}}}]}}]"),
            ]
        })
        .collect()
}

/// Dependency-free Bun plugin. Outside a Krill pane it registers no callbacks and spawns nothing.
/// Errors are swallowed so a broken/missing krill never blocks the agent.
pub fn opencode_plugin(exe: &Path) -> Option<String> {
    let exe = serde_json::Value::String(exe.to_str()?.to_owned()).to_string();
    Some(format!(
        r#"// Krill managed hook (MK-048).
export const KrillHook = async () => {{
  if (!process.env.MK_PANE_ID) return {{}};
  return {{ event: async ({{ event }}) => {{
    if (!process.env.MK_PANE_ID) return;
    const names = {{ "session.created": "SessionStart", "session.idle": "Stop",
      "session.deleted": "SessionEnd", "permission.asked": "PermissionRequest",
      "permission.updated": "PermissionRequest", "permission.replied": "PostToolUse",
      "question.asked": "PermissionRequest", "question.replied": "PostToolUse",
      "question.rejected": "PostToolUse" }};
    const name = event.type === "session.status" && event.properties?.status?.type === "busy"
      ? "UserPromptSubmit" : names[event.type];
    if (!name) return;
    try {{
      const child = Bun.spawn([{exe}, "hook", "opencode"], {{
        stdin: new TextEncoder().encode(JSON.stringify({{ hook_event_name: name,
          session_id: event.properties?.sessionID ?? event.properties?.info?.id }})),
        stdout: "ignore", stderr: "ignore"
      }});
      await child.exited;
    }} catch {{}}
  }} }};
}};
"#
    ))
}

/// Pi discovers user extensions in `$PI_CODING_AGENT_DIR/extensions`.
pub fn pi_extension_path(root: &Path) -> std::path::PathBuf {
    root.join("extensions/krill.ts")
}

/// Observational Pi extension. Spawn directly with JSON stdin, never through a
/// shell. Missing hooks cannot block Pi or answer its permission questions.
pub fn pi_extension(exe: &Path) -> Option<String> {
    let exe = serde_json::Value::String(exe.to_str()?.to_owned()).to_string();
    Some(format!(
        r#"// Krill managed Pi extension.
import {{ spawn }} from "node:child_process";
export default function (pi) {{
  if (!process.env.MK_PANE_ID) return;
  const events = {{ session_start: "SessionStart", agent_start: "UserPromptSubmit",
    agent_settled: "Stop", session_shutdown: "SessionEnd" }};
  for (const [event, name] of Object.entries(events)) {{
    pi.on(event, async (_event, ctx) => {{
      if (!process.env.MK_PANE_ID) return;
      try {{
        await new Promise((resolve) => {{
          const child = spawn({exe}, ["hook", "pi"], {{
            stdio: ["pipe", "ignore", "ignore"], timeout: 1000, killSignal: "SIGKILL", windowsHide: true
          }});
          child.on("error", () => resolve());
          child.on("close", () => resolve());
          child.stdin.on("error", () => {{}});
          child.stdin.end(JSON.stringify({{ hook_event_name: name,
            session_id: ctx.sessionManager.getSessionId() }}));
        }});
      }} catch {{}}
    }});
  }}
}}
"#
    ))
}

/// CLI(`krill hook install`)와 GUI(관리자 에이전트 탭)가 공유하는 JSON 머지. 같은 입력이면
/// 바이트 단위로 같은 결과를 낸다(`serde_json` 맵은 키 정렬). 파일 읽기·백업·원자 쓰기는
/// 호출자가 한다. 파싱 실패·비객체·예상 밖 타입은 오류로 돌려주고 호출자는 쓰지 않는다.
fn merge_error(message: impl Into<String>) -> io::Error {
    io::Error::other(message.into())
}

/// `None`(파일 없음)은 `{}`에서 시작한다.
pub fn parse_hook_json(before: Option<&str>) -> io::Result<serde_json::Value> {
    let value: serde_json::Value =
        serde_json::from_str(before.unwrap_or("{}")).map_err(io::Error::other)?;
    if !value.is_object() {
        return Err(merge_error("hook settings must be a JSON object"));
    }
    Ok(value)
}

pub fn encode_hook_json(value: &serde_json::Value) -> io::Result<String> {
    serde_json::to_string_pretty(value)
        .map(|s| s + "\n")
        .map_err(io::Error::other)
}

/// Claude command handler. Windows uses the documented exec form so executable
/// paths with spaces, quotes or shell metacharacters never pass through a shell.
/// Its CLI checks `MK_PANE_ID` before collecting or delivering an event.
pub fn claude_hook(command: &str) -> serde_json::Value {
    #[cfg(windows)]
    {
        json!({"type": "command", "command": command, "args": ["hook", "claude"]})
    }
    #[cfg(not(windows))]
    {
        json!({"type": "command", "command": command})
    }
}

/// Claude `settings.json` 머지. 항목 모양은 `{"hooks": [{"type": "command",
/// "name": "krill", "command": ...}]}`이며 이미 있으면 건드리지 않는다.
pub fn install_claude_json(before: Option<&str>, command: &str) -> io::Result<String> {
    let mut value = parse_hook_json(before)?;
    let root = value
        .as_object_mut()
        .ok_or_else(|| merge_error("settings must be an object"))?;
    let hooks = root
        .entry("hooks")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or_else(|| merge_error("hooks must be an object"))?;
    for event in super::claude::events(None) {
        let entries = hooks
            .entry(event)
            .or_insert_with(|| json!([]))
            .as_array_mut()
            .ok_or_else(|| merge_error(format!("hooks.{event} must be an array")))?;
        let mut handler = claude_hook(command);
        handler
            .as_object_mut()
            .ok_or_else(|| merge_error("Claude hook handler must be an object"))?
            .insert("name".into(), json!("krill"));
        let entry = json!({"hooks": [handler]});
        if !entries.contains(&entry) {
            entries.push(entry);
        }
    }
    encode_hook_json(&value)
}

/// Claude 외 에이전트 머지. Antigravity는 `krill` 묶음 키 아래에
/// `agy_event_command` 완성 명령(`hook agy <event>` 포함)을 넣는다.
pub fn install_agent_json(before: Option<&str>, agent: Agent, command: &str) -> io::Result<String> {
    if agent == Agent::Pi {
        return Err(merge_error("this agent uses an extension, not JSON hooks"));
    }
    let mut value = parse_hook_json(before)?;
    let root = value
        .as_object_mut()
        .ok_or_else(|| merge_error("settings must be an object"))?;
    if agent == Agent::Agy {
        let bundle = root
            .entry("krill")
            .or_insert(json!({}))
            .as_object_mut()
            .ok_or_else(|| merge_error("hooks.krill must be an object"))?;
        for &event in agent.events() {
            let entries = bundle
                .entry(event)
                .or_insert(json!([]))
                .as_array_mut()
                .ok_or_else(|| merge_error(format!("krill.{event} must be an array")))?;
            let ev_cmd = agy_event_command(command, event);
            let entry = if matches!(event, "PreToolUse" | "PostToolUse") {
                json!({
                    "matcher": "*",
                    "hooks": [{"type": "command", "command": ev_cmd, "timeout": 10}]
                })
            } else {
                json!({"type": "command", "command": ev_cmd, "timeout": 10})
            };
            if !entries.contains(&entry) {
                entries.push(entry);
            }
        }
        return encode_hook_json(&value);
    }
    if agent == Agent::Cursor {
        if root.get("version").is_some_and(|v| v != &json!(1)) {
            return Err(merge_error("unsupported Cursor hooks version"));
        }
        root.entry("version").or_insert(json!(1));
    }
    let hooks = root
        .entry("hooks")
        .or_insert(json!({}))
        .as_object_mut()
        .ok_or_else(|| merge_error("hooks must be an object"))?;
    for event in agent.events() {
        let entries = hooks
            .entry(*event)
            .or_insert(json!([]))
            .as_array_mut()
            .ok_or_else(|| merge_error(format!("hooks.{event} must be an array")))?;
        let entry = if agent == Agent::Cursor {
            json!({"command": command})
        } else {
            json!({"hooks": [{"type": "command", "name": "krill", "command": command}]})
        };
        // 영수증 없이 같은 항목이 있으면 사용자 소유로 보고 가져오지 않는다.
        if !entries.contains(&entry) {
            entries.push(entry);
        }
    }
    encode_hook_json(&value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_codex_command_encodes_literal_path_and_raw_stdin() {
        use base64::Engine as _;
        let command = windows_codex_command(r"C:\한글 '$ & %USERNAME%\krill.exe");
        let encoded = command.split_whitespace().last().unwrap();
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .unwrap();
        let utf16: Vec<_> = bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| u16::from_le_bytes(*pair))
            .collect();
        let script = String::from_utf16(&utf16).unwrap();
        assert!(script.contains(r"$info.FileName='C:\한글 ''$ & %USERNAME%\krill.exe'"));
        assert!(script.contains("$ErrorActionPreference='Stop'"));
        assert!(script.contains("if (-not $env:MK_PANE_ID)"));
        assert!(script.contains("OpenStandardInput().CopyTo($child.StandardInput.BaseStream)"));
        assert!(script.contains("exit $code"));
    }

    #[test]
    fn codex_session_keys_accept_native_roots_without_trusting_other_sources() {
        for key in [
            "/<session-flags>/config.toml:session_start:0:0",
            r"C:\<session-flags>\config.toml:session_start:0:0",
            "C:/<session-flags>/config.toml:session_start:0:0",
        ] {
            assert_eq!(codex_session_event(key), Some("session_start"));
        }
        for key in [
            r"C:\project\<session-flags>\config.toml:stop:0:0",
            "/project/config.toml:stop:0:0",
            "/<session-flags>/config.toml:stop:1:0",
            "/<session-flags>/config.toml:",
        ] {
            assert_eq!(codex_session_event(key), None);
        }
    }

    #[test]
    fn pi_hook_extension_is_observational_and_uses_direct_spawn() {
        let pi = Agent::parse("pi").unwrap();
        assert_eq!(pi.id(), "pi");
        assert!(pi.events().is_empty());
        assert_eq!(
            pi_extension_path(Path::new("agent")),
            Path::new("agent/extensions/krill.ts")
        );
        let extension = pi_extension(Path::new("/a path/'krill'")).unwrap();
        for event in [
            "session_start",
            "agent_start",
            "agent_settled",
            "session_shutdown",
        ] {
            assert!(extension.contains(event));
        }
        assert!(extension.contains("MK_PANE_ID"));
        assert!(extension.contains("timeout: 1000"));
        assert!(extension.contains(r#"["hook", "pi"]"#));
        assert!(extension.contains("child.stdin.end(JSON.stringify"));
        assert!(extension.contains("/a path/'krill'"));
        assert!(!extension.contains("shell: true"));
        assert!(!extension.contains("tool_call:"));
        assert!(install_agent_json(None, Agent::Pi, "krill").is_err());
    }

    #[test]
    fn codex_overrides_parse_as_toml_and_do_not_contain_pane_identity() {
        let cmd = command(Path::new("/tmp/한글 '$` krill"), Agent::Codex).unwrap();
        for pair in codex_args(&cmd).as_chunks::<2>().0 {
            let text = pair.get(1).unwrap();
            let parsed: toml::Value = toml::from_str(text).unwrap();
            assert!(parsed.get("hooks").is_some());
            assert!(!text.contains("auth.json"));
        }
    }
}

#[cfg(test)]
mod claude_platform_tests {
    #[test]
    fn windows_claude_exec_form_keeps_executable_metacharacters_literal() {
        let command = r"C:\한글 '$` & dir\krill.exe";
        let handler = super::claude_hook(command);
        assert_eq!(handler["command"], command);
        if cfg!(windows) {
            assert_eq!(handler["args"], serde_json::json!(["hook", "claude"]));
            assert!(handler.get("shell").is_none());
        } else {
            assert!(handler.get("args").is_none());
        }
    }
}
