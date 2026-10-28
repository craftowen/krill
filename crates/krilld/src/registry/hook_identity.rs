//! Attribute hooks to the foreground outer agent before replacing a shell's saved identity.

use krill_proto::hook::HookEvent;
#[cfg(any(test, target_os = "macos"))]
use krill_proto::usage::ProviderKind;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(
    not(any(test, target_os = "macos")),
    expect(dead_code, reason = "live foreground attribution is macOS-only")
)]
pub(super) enum Origin {
    Foreground,
    Auxiliary,
    Unverified,
}

#[cfg(target_os = "macos")]
pub(super) fn origin(root: u32, foreground: Option<u32>, event: &HookEvent) -> Origin {
    origin_with(root, foreground, event, |pid| {
        let start_us = crate::sys::start_us(pid).ok()?;
        let name = crate::sys::process_name(pid).ok()?;
        let parent = if pid == root {
            0
        } else {
            crate::sys::parent_pid(pid).ok()?
        };
        // A PID must identify the same process throughout the kernel queries.
        (crate::sys::start_us(pid).ok()? == start_us).then_some(Process {
            parent,
            name,
            start_us,
        })
    })
}

// Only macOS exposes process birth in the same epoch as HookEvent's Unix timestamp.
// Keep the established hook behavior elsewhere until equivalent attribution is available.
#[cfg(not(target_os = "macos"))]
pub(super) fn origin(_root: u32, _foreground: Option<u32>, _event: &HookEvent) -> Origin {
    Origin::Unverified
}

#[cfg(any(test, target_os = "macos"))]
struct Process {
    parent: u32,
    name: String,
    start_us: u64,
}

#[derive(Clone, Copy, PartialEq, Eq)]
#[cfg(any(test, target_os = "macos"))]
struct Agent {
    pid: u32,
    start_us: u64,
    provider: ProviderKind,
}

#[cfg(any(test, target_os = "macos"))]
fn provider(name: &str) -> Option<ProviderKind> {
    let name = name.rsplit(['/', '\\']).next()?;
    let name = name
        .rsplit_once('.')
        .filter(|(_, extension)| extension.eq_ignore_ascii_case("exe"))
        .map_or(name, |(stem, _)| stem);
    ProviderKind::parse(name)
}

#[cfg(any(test, target_os = "macos"))]
fn origin_with(
    root: u32,
    foreground: Option<u32>,
    event: &HookEvent,
    mut process: impl FnMut(u32) -> Option<Process>,
) -> Origin {
    let (Some(foreground), Some(expected)) = (foreground, provider(&event.agent)) else {
        return Origin::Unverified;
    };
    let event_us = event.id.unix_ns / 1_000;
    let Some((_, Some(outer))) = ancestry(root, foreground, event_us, &mut process) else {
        return Origin::Unverified;
    };
    let Some((Some(caller), _)) = ancestry(root, event.ppid, event_us, &mut process) else {
        return Origin::Unverified;
    };
    if caller == outer && caller.provider == expected {
        Origin::Foreground
    } else {
        Origin::Auxiliary
    }
}

/// Return nearest and outermost provider only after the entire ancestry reaches this root.
#[cfg(any(test, target_os = "macos"))]
fn ancestry(
    root: u32,
    mut pid: u32,
    event_us: u64,
    process: &mut impl FnMut(u32) -> Option<Process>,
) -> Option<(Option<Agent>, Option<Agent>)> {
    let mut seen = Vec::new();
    let mut child_start = None;
    let mut nearest = None;
    let mut outermost = None;
    for _ in 0..32 {
        if pid == 0 || seen.contains(&pid) {
            return None;
        }
        seen.push(pid);
        let current = process(pid)?;
        // Spool delivery can outlive the original caller. Its reused PID is not evidence.
        if current.start_us == 0
            || current.start_us > event_us
            || child_start.is_some_and(|child| current.start_us > child)
        {
            return None;
        }
        if let Some(provider) = provider(&current.name) {
            let agent = Agent {
                pid,
                start_us: current.start_us,
                provider,
            };
            nearest.get_or_insert(agent);
            outermost = Some(agent);
        }
        if pid == root {
            return Some((nearest, outermost));
        }
        child_start = Some(current.start_us);
        pid = current.parent;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use krill_proto::hook::{EventId, Input};

    #[test]
    #[cfg(target_os = "macos")]
    fn live_native_process_identity_uses_kernel_birth_time() {
        let root = std::env::temp_dir().join(format!("krill-hook-owner-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let executable = root.join("codex");
        std::fs::copy("/bin/sleep", &executable).unwrap();
        let mut child = std::process::Command::new(&executable)
            .arg("30")
            .spawn()
            .unwrap();
        let mut hook = event("codex", child.id());
        hook.id.unix_ns = u64::try_from(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        )
        .unwrap();
        let result = origin(child.id(), Some(child.id()), &hook);
        hook.id.unix_ns = 1;
        let stale = origin(child.id(), Some(child.id()), &hook);
        child.kill().unwrap();
        child.wait().unwrap();
        std::fs::remove_dir_all(root).unwrap();
        assert_eq!(result, Origin::Foreground);
        assert_eq!(stale, Origin::Unverified);
    }

    fn event(agent: &str, parent: u32) -> HookEvent {
        HookEvent {
            id: EventId {
                pid: 99,
                unix_ns: 100_000,
            },
            agent: agent.into(),
            ppid: parent,
            input: Input::Complete,
            payload: Vec::new(),
        }
    }

    fn classify(rows: &[(u32, u32, &str)], root: u32, fg: Option<u32>, e: &HookEvent) -> Origin {
        origin_with(root, fg, e, |pid| {
            rows.iter().find(|row| row.0 == pid).map(|row| Process {
                parent: row.1,
                name: row.2.into(),
                start_us: 1,
            })
        })
    }

    #[test]
    fn native_agent_root_and_shell_wrappers_are_foreground() {
        let native = [(10, 1, "claude"), (11, 10, "sh")];
        assert_eq!(
            classify(&native, 10, Some(10), &event("claude-code", 11)),
            Origin::Foreground
        );
        let wrapped = [
            (10, 1, "zsh"),
            (11, 10, "/opt/bin/codex"),
            (12, 11, "sh"),
            (13, 12, "bash"),
        ];
        assert_eq!(
            classify(&wrapped, 10, Some(12), &event("codex", 13)),
            Origin::Foreground
        );
        assert_eq!(
            classify(
                &[(10, 1, "C:\\bin\\Claude.EXE")],
                10,
                Some(10),
                &event("claude", 10)
            ),
            Origin::Foreground
        );
    }

    #[test]
    fn nested_agents_and_background_siblings_are_auxiliary() {
        for nested in ["codex", "claude"] {
            let rows = [
                (10, 1, "zsh"),
                (11, 10, "codex"),
                (12, 11, nested),
                (13, 12, "sh"),
            ];
            // Even when a nested tool owns the foreground group, the outer agent owns the pane.
            assert_eq!(
                classify(&rows, 10, Some(12), &event(nested, 13)),
                Origin::Auxiliary
            );
        }
        let rows = [(10, 1, "zsh"), (11, 10, "claude"), (12, 10, "claude")];
        assert_eq!(
            classify(&rows, 10, Some(11), &event("claude", 12)),
            Origin::Auxiliary
        );
        assert_eq!(
            classify(&rows, 10, Some(11), &event("codex", 11)),
            Origin::Auxiliary
        );
    }

    #[test]
    fn missing_foreground_dead_caller_and_unrelated_roots_are_unverified() {
        let rows = [(10, 1, "zsh"), (11, 10, "claude"), (12, 1, "claude")];
        for (fg, caller) in [(None, 11), (Some(11), 99), (Some(11), 12)] {
            assert_eq!(
                classify(&rows, 10, fg, &event("claude", caller)),
                Origin::Unverified
            );
        }
        assert_eq!(
            classify(&rows, 10, Some(11), &event("unknown", 11)),
            Origin::Unverified
        );
    }

    #[test]
    fn reused_pids_and_younger_parents_are_unverified() {
        for (caller_start, root_start) in [(101, 1), (1, 2)] {
            assert_eq!(
                origin_with(10, Some(11), &event("claude", 11), |pid| Some(Process {
                    parent: if pid == 11 { 10 } else { 1 },
                    name: if pid == 11 { "claude" } else { "zsh" }.into(),
                    start_us: if pid == 11 { caller_start } else { root_start },
                })),
                Origin::Unverified
            );
        }
    }

    #[test]
    fn cycles_and_excessive_ancestry_are_bounded() {
        let mut queries = 0;
        assert_eq!(
            origin_with(10, Some(11), &event("claude", 11), |pid| {
                queries += 1;
                Some(Process {
                    parent: if pid == 11 { 12 } else { 11 },
                    name: "claude".into(),
                    start_us: 1,
                })
            }),
            Origin::Unverified
        );
        assert_eq!(queries, 2);
        queries = 0;
        assert_eq!(
            origin_with(100, Some(1), &event("claude", 1), |pid| {
                queries += 1;
                Some(Process {
                    parent: pid.saturating_add(1),
                    name: "claude".into(),
                    start_us: 1,
                })
            }),
            Origin::Unverified
        );
        assert_eq!(queries, 32);
    }
}
