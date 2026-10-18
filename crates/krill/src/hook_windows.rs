//! Windows hook 전달은 네이티브 stdin·부모 PID 구현 전까지 fail-open으로 거절한다.
use std::io::{self, Write as _};
use std::process::ExitCode;

pub fn run(agent: Option<&str>, event_arg: Option<&str>) -> ExitCode {
    let event = event_arg
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .or_else(|| std::env::var("MK_HOOK_EVENT").ok());
    // Unix hook과 같은 응답 계약을 유지한다. 전달 실패가 에이전트를 막으면 안 된다.
    let response: &[u8] = if agent.is_some_and(|a| matches!(a, "agy" | "antigravity")) {
        match event.as_deref() {
            Some("PreToolUse") => b"{\"decision\":\"ask\"}\n",
            Some("Stop") => b"{\"decision\":\"\"}\n",
            _ => b"{}\n",
        }
    } else {
        b"{}\n"
    };
    let mut out = io::stdout().lock();
    out.write_all(response).and_then(|()| out.flush()).ok();
    drop(out);
    eprintln!(
        "krill hook: {}",
        std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "hook delivery is not supported on Windows yet",
        )
    );
    ExitCode::SUCCESS
}
