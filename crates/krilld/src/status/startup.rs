//! Agent launches run the user's login shell first (`zsh -l -i -c PREFLIGHT mk-agent ...`).
//! A hung startup file (`eval "$(brew shellenv)"` blocked on a TCC check, a dead network
//! mount) leaves the terminal echoing input with no agent behind it. Report what is stuck.

#[cfg(unix)]
use std::time::Duration;

/// Startup files get this long before a launch that still has no agent counts as stuck.
#[cfg(unix)]
pub(crate) const STARTUP_GRACE: Duration = Duration::from_secs(10);
#[cfg(unix)]
const MAX_SHOWN: usize = 120;

/// `root` is the session's root process. Only wrapper roots whose agent has not
/// started after [`STARTUP_GRACE`] report the stuck command.
#[cfg(unix)]
pub(crate) fn startup_stall(root: u32) -> Option<String> {
    stall_after(root, STARTUP_GRACE)
}

#[cfg(unix)]
fn stall_after(root: u32, grace: Duration) -> Option<String> {
    let argv = crate::sys::process_args(root).ok()?;
    if !crate::registry::is_wrapper_argv(&argv) || crate::sys::process_age(root).ok()? < grace {
        return None;
    }
    let children: Vec<_> = crate::sys::child_pids(root)
        .ok()?
        .into_iter()
        .filter_map(|pid| crate::sys::process_args(pid).ok())
        .filter(|args| !args.is_empty())
        .collect();
    stall(&argv, &children)
}

#[cfg(not(unix))]
pub(crate) fn startup_stall(_root: u32) -> Option<String> {
    None
}

/// `root_argv` is a wrapper argv (`agent` starts at index 7). `children` are the
/// root's direct children. The agent counts as started once a child runs its command:
/// the forked `( exec "$@" )` subshell, the exec'd agent, or a shebang interpreter.
#[cfg(unix)]
pub(crate) fn stall(root_argv: &[String], children: &[Vec<String>]) -> Option<String> {
    let command = root_argv.get(7..).filter(|c| !c.is_empty())?;
    if children.iter().any(|child| launched(child, command)) {
        return None;
    }
    Some(match children.first() {
        Some(child) => describe(child),
        None => base(root_argv.first()?).trim_start_matches('-').to_owned(),
    })
    .filter(|shown| !shown.is_empty())
}

#[cfg(unix)]
fn launched(child: &[String], command: &[String]) -> bool {
    let (agent, args) = command
        .split_first()
        .map_or(("", &[][..]), |(a, r)| (a.as_str(), r));
    child.ends_with(command)
        || (!args.is_empty() && child.ends_with(args))
        || child.iter().take(3).any(|arg| base(arg) == base(agent))
}

/// `/bin/bash -pu /opt/homebrew/bin/brew shellenv zsh` → `brew shellenv zsh`.
#[cfg(unix)]
fn describe(args: &[String]) -> String {
    let mut rest = args;
    while let [first, tail @ ..] = rest
        && matches!(base(first), "sh" | "bash" | "zsh" | "dash" | "fish" | "env")
    {
        let skip = tail.iter().take_while(|a| a.starts_with('-')).count();
        match tail.get(skip..) {
            Some(next) if !next.is_empty() => rest = next,
            _ => break,
        }
    }
    let mut shown = rest
        .iter()
        .enumerate()
        .map(|(i, a)| if i == 0 { base(a) } else { a.as_str() })
        .collect::<Vec<_>>()
        .join(" ");
    if let Some((cut, _)) = shown.char_indices().nth(MAX_SHOWN) {
        shown.truncate(cut);
        shown.push('…');
    }
    shown
}

#[cfg(unix)]
fn base(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|&s| s.to_owned()).collect()
    }

    fn root() -> Vec<String> {
        argv(&[
            "/bin/zsh",
            "-l",
            "-i",
            "-c",
            "PREFLIGHT",
            crate::agent::WRAPPER_ARG0,
            "hint",
            "claude",
            "--session-id",
            "abc",
        ])
    }

    #[test]
    fn hung_startup_file_reports_the_stuck_command() {
        let brew = argv(&[
            "/bin/bash",
            "-pu",
            "/opt/homebrew/bin/brew",
            "shellenv",
            "zsh",
        ]);
        assert_eq!(
            stall(&root(), &[brew]).as_deref(),
            Some("brew shellenv zsh")
        );
        // The shell itself blocked (a builtin `[[ -r $PWD ]]`) with no child.
        assert_eq!(stall(&root(), &[]).as_deref(), Some("zsh"));
    }

    #[test]
    fn started_agent_is_not_a_stall() {
        let exec = argv(&["claude", "--session-id", "abc"]);
        let subshell = root();
        let shebang = argv(&["node", "/opt/homebrew/bin/claude", "--session-id", "abc"]);
        let native = argv(&["/Users/u/.local/share/claude/versions/2.1.319"]);
        for child in [exec, subshell, shebang] {
            assert_eq!(stall(&root(), &[child]), None);
        }
        // A renamed native binary still carries the launch arguments.
        let mut native = native;
        native.extend(argv(&["--session-id", "abc"]));
        assert_eq!(stall(&root(), &[native]), None);
    }

    #[test]
    fn agent_without_arguments_matches_by_name() {
        let mut bare = root();
        bare.truncate(8);
        assert_eq!(stall(&bare, &[argv(&["/usr/local/bin/claude"])]), None);
        assert_eq!(
            stall(&bare, &[argv(&["sleep", "30"])]).as_deref(),
            Some("sleep 30")
        );
    }

    #[expect(clippy::unwrap_used, reason = "test fixture spawns /bin/sh")]
    fn wrapper(script: &str, agent: &[&str]) -> std::process::Child {
        std::process::Command::new("/bin/sh")
            .args(["-e", "-u", "-c", script, crate::agent::WRAPPER_ARG0, "hint"])
            .args(agent)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap()
    }

    /// Poll until the wrapper has forked its child, then read the verdict.
    fn verdict(child: &mut std::process::Child) -> Option<String> {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while crate::sys::child_pids(child.id())
            .unwrap_or_default()
            .is_empty()
            && std::time::Instant::now() < deadline
        {
            std::thread::sleep(Duration::from_millis(20));
        }
        std::thread::sleep(Duration::from_millis(100));
        let found = stall_after(child.id(), Duration::ZERO);
        reap(child);
        found
    }

    fn reap(child: &mut std::process::Child) {
        for pid in crate::sys::child_pids(child.id()).unwrap_or_default() {
            if let Some(pid) = rustix::process::Pid::from_raw(i32::try_from(pid).unwrap_or(0)) {
                rustix::process::kill_process(pid, rustix::process::Signal::KILL).ok();
            }
        }
        child.kill().ok();
        child.wait().ok();
    }

    #[test]
    fn live_wrapper_stuck_in_startup_is_reported_until_the_agent_runs() {
        // A startup command that never returns, before the agent line runs.
        let mut stuck = wrapper("sleep 30; :", &["claude", "--x"]);
        assert_eq!(verdict(&mut stuck).as_deref(), Some("sleep 30"));
        // The agent itself running in the wrapper's subshell is not a stall.
        let mut started = wrapper("shift; (\"$@\"); :", &["sleep", "30"]);
        assert_eq!(verdict(&mut started), None);
        // Within the grace period nothing is reported.
        let mut young = wrapper("sleep 30; :", &["claude"]);
        assert_eq!(stall_after(young.id(), Duration::from_secs(60)), None);
        reap(&mut young);
    }

    #[test]
    fn long_commands_are_cut_on_a_char_boundary() {
        let long = vec!["가".repeat(200)];
        let shown = stall(&root(), &[long]).unwrap();
        assert_eq!(shown.chars().count(), MAX_SHOWN + 1);
        assert!(shown.ends_with('…'));
    }
}
