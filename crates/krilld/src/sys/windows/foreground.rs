//! Reuse the existing bounded-lifetime Toolhelp cache; never spawn a process to identify agents.

pub(crate) fn session_agent_process(root: u32) -> Option<u32> {
    let tree = super::processes(None)
        .ok()?
        .into_iter()
        .map(|process| (process.pid, process.parent, process.exe))
        .collect::<Vec<_>>();
    crate::status::process_tree::session_agent(root, &tree, |pid| super::start_us(pid).ok())
}
