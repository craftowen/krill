//! Windows has no PTY foreground group. Use only native agent executables whose entire
//! parent chain reaches the session root, with creation times checked against PID reuse.

use std::collections::BTreeMap;

fn native_agent(name: &str) -> bool {
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
pub(crate) fn session_agent(
    root: u32,
    processes: &[(u32, u32, String)],
    mut start_us: impl FnMut(u32) -> Option<u64>,
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
        if pid == root || !native_agent(name) {
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
