//! Windows has no PTY foreground group. Use only native agent executables whose entire
//! parent chain reaches the session root, with creation times checked against PID reuse.

use std::collections::{BTreeMap, BTreeSet};

/// Cheap prefilter only; identity and console membership still require validation.
pub(crate) fn has_session_agent(root: u32, processes: &[(u32, u32, String)]) -> bool {
    let mut descendants = BTreeSet::from([root]);
    for _ in 0..64 {
        let before = descendants.len();
        for (pid, parent, _) in processes {
            if descendants.contains(parent) {
                descendants.insert(*pid);
            }
        }
        if descendants.len() == before {
            break;
        }
    }
    processes
        .iter()
        .any(|(pid, _, name)| *pid != root && descendants.contains(pid) && native_agent(name))
}

pub(crate) fn native_agent(name: &str) -> bool {
    let name = name.rsplit(['/', '\\']).next().unwrap_or(name);
    let name = name
        .rsplit_once('.')
        .filter(|(_, ext)| ext.eq_ignore_ascii_case("exe"))
        .map_or(name, |(stem, _)| stem);
    [
        "claude",
        "codex",
        "opencode",
        "cursor-agent",
        "agy",
        "antigravity",
        "pi",
    ]
    .iter()
    .any(|agent| name.eq_ignore_ascii_case(agent))
}

/// Prefer the outer agent over agents launched as its tools. Parallel agent branches are
/// ambiguous, so retain hook identity rather than choosing an unrelated background task.
#[cfg(test)]
pub(crate) fn session_agent(
    root: u32,
    processes: &[(u32, u32, String)],
    start_us: impl FnMut(u32) -> Option<u64>,
) -> Option<u32> {
    session_agent_in_console(root, processes, start_us, |_| true)
}

pub(crate) fn session_agent_in_console(
    root: u32,
    processes: &[(u32, u32, String)],
    mut start_us: impl FnMut(u32) -> Option<u64>,
    mut attached: impl FnMut(u32) -> bool,
) -> Option<u32> {
    let tree: BTreeMap<_, _> = processes
        .iter()
        .map(|(pid, parent, name)| (*pid, (*parent, name.as_str())))
        .collect();
    if root == 0 || !tree.contains_key(&root) {
        return None;
    }
    let mut starts = BTreeMap::new();
    let mut started = |pid| *starts.entry(pid).or_insert_with(|| start_us(pid));
    let mut found = None;
    for (&pid, &(_, name)) in &tree {
        if pid == root || !native_agent(name) || !attached(pid) {
            continue;
        }
        let mut current = pid;
        let mut birth = started(current);
        // Cap ancestry even for cyclic or malformed snapshots. Real launch chains are short.
        for _ in 0..64 {
            let Some(&(parent, _)) = tree.get(&current) else {
                break;
            };
            let parent_birth = started(parent);
            if !matches!((parent_birth, birth), (Some(p), Some(c)) if p <= c) {
                break;
            }
            if parent == root {
                if found.is_some() {
                    return None;
                }
                found = Some(pid);
                break;
            }
            if tree
                .get(&parent)
                .is_some_and(|(_, name)| native_agent(name))
            {
                break;
            }
            current = parent;
            birth = parent_birth;
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(rows: &[(u32, u32, &str)]) -> Vec<(u32, u32, String)> {
        rows.iter()
            .map(|&(pid, parent, name)| (pid, parent, name.into()))
            .collect()
    }

    #[test]
    fn reused_shell_detects_codex_through_npm_shims_and_excludes_other_sessions() {
        let mut rows = tree(&[
            (10, 1, "powershell.exe"),
            (20, 10, "cmd.exe"),
            (30, 20, "node.exe"),
            (40, 30, "CODEX.EXE"),
            (50, 1, "claude.exe"),
        ]);
        assert_eq!(
            session_agent(10, &rows, |pid| Some(u64::from(pid))),
            Some(40)
        );
        rows.retain(|(pid, _, _)| *pid != 40);
        assert_eq!(session_agent(10, &rows, |pid| Some(u64::from(pid))), None);
        rows.push((60, 20, "claude.exe".into()));
        assert_eq!(
            session_agent(10, &rows, |pid| Some(u64::from(pid))),
            Some(60)
        );
    }

    #[test]
    fn nested_agent_tools_do_not_replace_the_outer_agent_and_parallel_branches_are_ambiguous() {
        let mut rows = tree(&[
            (10, 1, "pwsh.exe"),
            (20, 10, "claude.exe"),
            (30, 20, "codex.exe"),
        ]);
        assert_eq!(
            session_agent(10, &rows, |pid| Some(u64::from(pid))),
            Some(20)
        );
        rows.push((40, 10, "opencode.exe".into()));
        assert_eq!(session_agent(10, &rows, |pid| Some(u64::from(pid))), None);
    }

    #[test]
    fn pid_reuse_missing_parents_unreadable_births_and_cycles_never_guess() {
        let rows = tree(&[
            (10, 1, "pwsh.exe"),
            (20, 10, "cmd.exe"),
            (30, 20, "codex.exe"),
        ]);
        assert_eq!(
            session_agent(10, &rows, |pid| Some(if pid == 10 {
                99
            } else {
                u64::from(pid)
            })),
            None
        );
        assert_eq!(
            session_agent(10, &rows, |pid| (pid != 20).then_some(u64::from(pid))),
            None
        );
        assert_eq!(session_agent(0, &rows, |_| Some(1)), None);
        let rows = tree(&[
            (10, 1, "pwsh.exe"),
            (20, 30, "cmd.exe"),
            (30, 20, "codex.exe"),
        ]);
        assert_eq!(session_agent(10, &rows, |_| Some(1)), None);
        let rows = tree(&[(10, 1, "pwsh.exe"), (30, 20, "codex.exe")]);
        assert_eq!(session_agent(10, &rows, |_| Some(1)), None);
    }
}

#[cfg(test)]
mod console_tests {
    use super::*;

    #[test]
    fn detached_agent_does_not_win_or_make_attached_agent_ambiguous() {
        let processes = vec![
            (10, 1, "pwsh.exe".into()),
            (20, 10, "codex.exe".into()),
            (30, 10, "claude.exe".into()),
        ];
        assert_eq!(
            session_agent_in_console(10, &processes, |pid| Some(u64::from(pid)), |pid| pid == 20),
            Some(20)
        );
        assert_eq!(
            session_agent_in_console(10, &processes, |pid| Some(u64::from(pid)), |_| false),
            None
        );
        assert_eq!(
            session_agent_in_console(10, &processes, |pid| Some(u64::from(pid)), |_| true),
            None
        );
    }

    #[test]
    fn another_sessions_agent_does_not_trigger_console_probing() {
        let processes = vec![(10, 1, "pwsh.exe".into()), (20, 1, "codex.exe".into())];
        assert!(!has_session_agent(10, &processes));
        let processes = vec![
            (10, 1, "pwsh.exe".into()),
            (20, 10, "cmd.exe".into()),
            (30, 20, "codex.exe".into()),
        ];
        assert!(has_session_agent(10, &processes));
    }
}
