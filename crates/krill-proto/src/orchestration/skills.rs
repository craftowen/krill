//! Bundled skills and owned, idempotent skill synchronization.

use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use super::{LockGuard, invalid, not_found};
#[cfg(unix)]
use crate::os::fs::OpenOptionsExt as _;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillEntry {
    pub name: &'static str,
    pub description: &'static str,
    pub content: &'static str,
}

pub const BUILTIN_SKILLS: &[SkillEntry] = &[
    SkillEntry {
        name: "artifacts",
        description: "Create documents, reports, plans, charts, and interactive visual deliverables as Krill artifacts by default; publish and update them with the krill CLI.",
        content: include_str!("skills/artifacts/SKILL.md"),
    },
    SkillEntry {
        name: "krill",
        description: "Operate Krill sessions, worktrees, checkpoints, references, handoffs, and ports via the `krill` CLI.",
        content: "---\nname: krill\ndescription: Operate Krill sessions, worktrees, checkpoints, references, handoffs, and ports via the krill CLI.\n---\n\n# Krill (`krill`) CLI Skill\n\n- List/manage sessions: `krill ls --json`, `krill new --cwd <dir> --agent <profile>` (or `krill new --cwd <dir> -- <program> <args>...`), `krill send <session_id> --text \"<text>\" --enter`, `krill read <session_id> --lines 80`, `krill wait <session_id> --for idle --timeout 30`, `krill kill <session_id>`\n- Worktree checkpoints: `krill set --comment \"...\" --workspace-status in-progress --pr 42 --issue OWE-100 --reference https://...`\n- References: `krill reference list`, `krill reference add <url>`, `krill reference find <query>`\n- Handoff context to another agent: `krill handoff --from <id> --to-agent codex --prompt \"Continue verification\"`\n- Detect listening ports: `krill ports --json`\n- Send a file to the user's phone (Krill Drop; the phone fetches it when it next connects, kept 7 days): `krill drop <file>`\n",
    },
    SkillEntry {
        name: "orchestration",
        description: "Coordinate multi-agent runs, task DAGs, worker dispatches, threaded messages, blocking ask/reply, and decision gates via `krill orchestration`.",
        content: include_str!("skills/orchestration/SKILL.md"),
    },
    SkillEntry {
        name: "worktree-checkpoints",
        description: "Track worktree progress, status badges (todo/in-progress/in-review/completed), PR/issue links, and URL references.",
        content: "---\nname: worktree-checkpoints\ndescription: Track worktree progress, status badges, PR/issue links, and URL references.\n---\n\n# Krill Worktree Checkpoints & References\n\n- Update checkpoint state:\n  `krill set --workspace-status in-review --comment \"Ready for QA\" --pr 128 --issue OWE-626 --unread`\n- Clear unread dot:\n  `krill set --read`\n- Attach reference links:\n  `krill reference add https://github.com/org/repo/pull/128`\n  `krill reference list --json`\n",
    },
    SkillEntry {
        name: "computer-use",
        description: "Operate desktop applications on macOS, Windows, and Linux through native accessibility trees using the bundled agent-desktop CLI.",
        content: include_str!("skills/computer-use/SKILL.md"),
    },
    SkillEntry {
        name: "krig",
        description: "Route tasks across recon, align, design, model, drive, demock, slim, verify, ship, eval, backprop, and resume playbooks with automated verification gates.",
        content: include_str!("skills/krig/SKILL.md"),
    },
];

pub fn builtin_skills() -> &'static [SkillEntry] {
    BUILTIN_SKILLS
}

pub const ORCHESTRATION_REFERENCES: &[(&str, &str)] = &[
    (
        "references/coordinator-loop.md",
        include_str!("skills/orchestration/references/coordinator-loop.md"),
    ),
    (
        "references/worker-contract.md",
        include_str!("skills/orchestration/references/worker-contract.md"),
    ),
    (
        "references/messaging-and-gates.md",
        include_str!("skills/orchestration/references/messaging-and-gates.md"),
    ),
    (
        "references/recovery-and-cleanup.md",
        include_str!("skills/orchestration/references/recovery-and-cleanup.md"),
    ),
    (
        "references/placement-and-remote.md",
        include_str!("skills/orchestration/references/placement-and-remote.md"),
    ),
    (
        "references/planning-and-routing.md",
        include_str!("skills/orchestration/references/planning-and-routing.md"),
    ),
    (
        "references/verification-and-evidence.md",
        include_str!("skills/orchestration/references/verification-and-evidence.md"),
    ),
];

pub const KRIG_FILES: &[(&str, &str)] = &[
    (
        "agents/openai.yaml",
        include_str!("skills/krig/agents/openai.yaml"),
    ),
    (
        "playbooks/align.md",
        include_str!("skills/krig/playbooks/align.md"),
    ),
    (
        "playbooks/backprop.md",
        include_str!("skills/krig/playbooks/backprop.md"),
    ),
    (
        "playbooks/demock.md",
        include_str!("skills/krig/playbooks/demock.md"),
    ),
    (
        "playbooks/design.md",
        include_str!("skills/krig/playbooks/design.md"),
    ),
    (
        "playbooks/drive.md",
        include_str!("skills/krig/playbooks/drive.md"),
    ),
    (
        "playbooks/eval.md",
        include_str!("skills/krig/playbooks/eval.md"),
    ),
    (
        "playbooks/model.md",
        include_str!("skills/krig/playbooks/model.md"),
    ),
    (
        "playbooks/recon.md",
        include_str!("skills/krig/playbooks/recon.md"),
    ),
    (
        "playbooks/resume.md",
        include_str!("skills/krig/playbooks/resume.md"),
    ),
    (
        "playbooks/ship.md",
        include_str!("skills/krig/playbooks/ship.md"),
    ),
    (
        "playbooks/slim.md",
        include_str!("skills/krig/playbooks/slim.md"),
    ),
    (
        "playbooks/verify.md",
        include_str!("skills/krig/playbooks/verify.md"),
    ),
    (
        "references/deepening.md",
        include_str!("skills/krig/references/deepening.md"),
    ),
    (
        "references/leakage-8.md",
        include_str!("skills/krig/references/leakage-8.md"),
    ),
    (
        "references/standards-baseline.md",
        include_str!("skills/krig/references/standards-baseline.md"),
    ),
    (
        "scripts/ab-eval.mjs",
        include_str!("skills/krig/scripts/ab-eval.mjs"),
    ),
    (
        "scripts/checkpoint.mjs",
        include_str!("skills/krig/scripts/checkpoint.mjs"),
    ),
    (
        "scripts/decision-log.mjs",
        include_str!("skills/krig/scripts/decision-log.mjs"),
    ),
    (
        "scripts/ledger.mjs",
        include_str!("skills/krig/scripts/ledger.mjs"),
    ),
    (
        "scripts/lib/cli.mjs",
        include_str!("skills/krig/scripts/lib/cli.mjs"),
    ),
    (
        "scripts/lib/git-state.mjs",
        include_str!("skills/krig/scripts/lib/git-state.mjs"),
    ),
    (
        "scripts/lib/markdown.mjs",
        include_str!("skills/krig/scripts/lib/markdown.mjs"),
    ),
    (
        "scripts/lib/source-lex.mjs",
        include_str!("skills/krig/scripts/lib/source-lex.mjs"),
    ),
    (
        "scripts/recon.mjs",
        include_str!("skills/krig/scripts/recon.mjs"),
    ),
    (
        "scripts/route-table.mjs",
        include_str!("skills/krig/scripts/route-table.mjs"),
    ),
    (
        "scripts/route.mjs",
        include_str!("skills/krig/scripts/route.mjs"),
    ),
    (
        "scripts/scan.mjs",
        include_str!("skills/krig/scripts/scan.mjs"),
    ),
    (
        "scripts/ship-check.mjs",
        include_str!("skills/krig/scripts/ship-check.mjs"),
    ),
    (
        "scripts/slim.mjs",
        include_str!("skills/krig/scripts/slim.mjs"),
    ),
    (
        "scripts/statecheck.mjs",
        include_str!("skills/krig/scripts/statecheck.mjs"),
    ),
    (
        "scripts/unwrap.mjs",
        include_str!("skills/krig/scripts/unwrap.mjs"),
    ),
];

#[must_use]
pub fn canonical_skill_name(raw: &str) -> &str {
    let trimmed = raw.trim();
    let stripped = trimmed
        .strip_prefix("krill-")
        .or_else(|| trimmed.strip_prefix("mk-"))
        .unwrap_or(trimmed);
    match stripped {
        "agent-desktop" => "computer-use",
        other => other,
    }
}

pub fn skill_references(name: &str) -> &'static [(&'static str, &'static str)] {
    match canonical_skill_name(name) {
        "orchestration" => ORCHESTRATION_REFERENCES,
        "krig" => KRIG_FILES,
        _ => &[],
    }
}

// Orca's discovery entries must keep their own names and update ownership.
pub(super) const ORCA_SKILL_NAMES: &[&str] = &["orchestration", "computer-use"];
// Explicit slash-invoked skills must not create duplicate `krill-<name>` copies next to developer symlinks.
const NO_PREFIX_FALLBACK_SKILLS: &[&str] = &["krig"];
const MARKER: &str = "<!-- krill-managed-skill -->";
const JS_MARKER: &str = "// krill-managed-skill";
const YAML_MARKER: &str = "# krill-managed-skill";

fn marker_for_path(path: &Path) -> &'static str {
    match path.extension().and_then(|ext| ext.to_str()) {
        Some("mjs" | "js" | "cjs") => JS_MARKER,
        Some("yaml" | "yml") => YAML_MARKER,
        _ => MARKER,
    }
}
static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Global discovery roots. agy's CLI-specific root and shared App/IDE root are distinct.
/// Sources: <https://antigravity.google/docs/skills#cli-skill-locations> and the
/// agy 1.4.3 bundled customization guide (shared `.gemini/config/skills`).
/// pi supports Agent Skills locations (`~/.agents/skills/`) and the agent directory
/// (`~/.pi/agent/skills/`); opencode also reads the shared `~/.agents/skills/`.
/// Sources: pi `docs/skills.md` + `docs/configuration.md`, opencode `docs/skills/`.
pub fn skill_directories(home: &Path, agent: &str) -> Vec<PathBuf> {
    let roots: &[&str] = match agent {
        "claude" => &[".claude/skills"],
        "codex" => &[".codex/skills"],
        "agy" | "antigravity" => &[".gemini/antigravity-cli/skills", ".gemini/config/skills"],
        "cursor" => &[".cursor/skills"],
        "opencode" => &[".config/opencode/skills", ".agents/skills"],
        "pi" => &[".agents/skills", ".pi/agent/skills"],
        "all" => &[
            ".claude/skills",
            ".codex/skills",
            ".gemini/antigravity-cli/skills",
            ".gemini/config/skills",
            ".cursor/skills",
            ".config/opencode/skills",
            ".agents/skills",
            ".pi/agent/skills",
        ],
        _ => &[],
    };
    roots.iter().map(|p| home.join(p)).collect()
}

#[derive(Debug, Default)]
pub struct SkillSyncReport {
    pub installed: Vec<PathBuf>,
    pub updated: usize,
    pub unchanged: usize,
    pub failures: Vec<String>,
    /// Roots another installer (app, daemon, or CLI) was writing; it publishes the same bundle,
    /// and every later launch or session start retries, so this is expected, not a failure.
    pub busy: Vec<PathBuf>,
}

/// Best-effort synchronization: one foreign or unwritable root does not block other agents.
pub fn sync_builtin_skills(directories: &[PathBuf]) -> SkillSyncReport {
    let mut report = SkillSyncReport::default();
    for dir in directories {
        for skill in builtin_skills() {
            if NO_PREFIX_FALLBACK_SKILLS.contains(&skill.name)
                && ensure_directory_safe(dir).is_ok()
                && fs::symlink_metadata(dir.join(skill.name))
                    .is_ok_and(|meta| meta.file_type().is_symlink())
            {
                report.unchanged = report.unchanged.saturating_add(1);
                continue;
            }
            match install(skill, dir) {
                Ok((path, changed)) => {
                    report.installed.push(path);
                    if changed {
                        report.updated = report.updated.saturating_add(1);
                    } else {
                        report.unchanged = report.unchanged.saturating_add(1);
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    report.busy.push(dir.clone());
                    break;
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::PermissionDenied | io::ErrorKind::ReadOnlyFilesystem
                    ) || (cfg!(unix) && error.raw_os_error() == Some(1)) =>
                {
                    break;
                }
                Err(error) => {
                    report
                        .failures
                        .push(format!("{} ({}): {error}", skill.name, dir.display()));
                }
            }
        }
    }
    report
}

pub fn sync_builtin_skills_at(home: &Path) -> SkillSyncReport {
    sync_builtin_skills(&skill_directories(home, "all"))
}

pub fn install_skill(name: &str, base_dir: &Path) -> io::Result<PathBuf> {
    let canonical = canonical_skill_name(name);
    let skill = builtin_skills()
        .iter()
        .find(|s| s.name == canonical)
        .ok_or_else(|| not_found(format!("unknown skill: {}", name.trim())))?;
    install(skill, base_dir).map(|(path, _)| path)
}

fn prefixed_content(skill: &SkillEntry, prefix: &str) -> String {
    skill.content.replacen(
        &format!("name: {}\n", skill.name),
        &format!("name: {prefix}{}\n", skill.name),
        1,
    )
}

fn owned(existing: &str, skill: &SkillEntry) -> bool {
    existing.contains(MARKER)
        || existing == skill.content
        || existing == prefixed_content(skill, "mk-")
        || existing == prefixed_content(skill, "krill-")
}

fn candidate_writable(dir: &Path, skill: &SkillEntry) -> io::Result<bool> {
    match ensure_directory_safe(dir) {
        Ok(()) => {}
        Err(e)
            if matches!(
                e.kind(),
                io::ErrorKind::InvalidInput | io::ErrorKind::AlreadyExists
            ) =>
        {
            return Ok(false);
        }
        Err(e) => return Err(e),
    }
    let path = dir.join("SKILL.md");
    if !regular_or_missing(&path)? {
        return Ok(false);
    }
    match fs::read_to_string(path) {
        Ok(existing) => Ok(owned(&existing, skill)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(true),
        Err(e) => Err(e),
    }
}

fn select_writable_dir(skill: &SkillEntry, base_dir: &Path) -> io::Result<Option<(PathBuf, bool)>> {
    if NO_PREFIX_FALLBACK_SKILLS.contains(&skill.name) {
        let dir = base_dir.join(skill.name);
        return Ok(candidate_writable(&dir, skill)?.then_some((dir, false)));
    }
    let orca_owned = ORCA_SKILL_NAMES.contains(&skill.name);
    let candidates = [skill.name.to_owned(), format!("krill-{}", skill.name)];
    for (index, name) in candidates.iter().enumerate().skip(usize::from(orca_owned)) {
        let dir = base_dir.join(name);
        if candidate_writable(&dir, skill)? {
            return Ok(Some((dir, index != 0)));
        }
    }
    Ok(None)
}

fn already_current(skill: &SkillEntry, base_dir: &Path) -> io::Result<Option<PathBuf>> {
    let orca_owned = ORCA_SKILL_NAMES.contains(&skill.name);
    let Some((dir, prefixed)) = select_writable_dir(skill, base_dir)? else {
        return Ok(None);
    };
    let mut stale = vec![base_dir.join(format!("mk-{}", skill.name))];
    if orca_owned {
        stale.push(base_dir.join(skill.name));
    }
    if stale
        .into_iter()
        .any(|old| old != dir && old.join("SKILL.md").is_file())
    {
        return Ok(None);
    }
    let body = if prefixed {
        prefixed_content(skill, "krill-")
    } else {
        skill.content.to_owned()
    };
    let entry = dir.join("SKILL.md");
    let expected_entry = format!("{}\n{MARKER}\n", body.trim_end());
    if !regular_or_missing(&entry)?
        || fs::read(&entry).ok().as_deref() != Some(expected_entry.as_bytes())
    {
        return Ok(None);
    }
    let references = skill_references(skill.name);
    if references.is_empty() {
        if dir.join("references").exists() {
            return Ok(None);
        }
        return Ok(Some(entry));
    }
    for (relative, content) in references {
        let path = dir.join(relative);
        let Some(parent) = path.parent() else {
            return Ok(None);
        };
        if ensure_directory_safe(parent).is_err() || !regular_or_missing(&path)? {
            return Ok(None);
        }
        let marker = marker_for_path(&path);
        let expected = format!("{}\n{marker}\n", content.trim_end());
        if fs::read(&path).ok().as_deref() != Some(expected.as_bytes()) {
            return Ok(None);
        }
    }
    Ok(Some(entry))
}

fn install(skill: &SkillEntry, base_dir: &Path) -> io::Result<(PathBuf, bool)> {
    ensure_directory_safe(base_dir)?;
    let lock = base_dir.join(".krill-skills.lock");
    if !regular_or_missing(&lock)? {
        return Err(invalid("skill lock must not be a symlink"));
    }
    if let Some(entry) = already_current(skill, base_dir)? {
        return Ok((entry, false));
    }
    fs::create_dir_all(base_dir)?;
    let _lock = skill_lock(&lock)?;
    let orca_owned = ORCA_SKILL_NAMES.contains(&skill.name);
    let (dir, prefixed) = select_writable_dir(skill, base_dir)?.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::AlreadyExists,
            "skill paths belong to external skills or symlinks",
        )
    })?;
    // Preflight the whole bundle before replacing its entrypoint. Unknown files remain untouched.
    let mut files = Vec::new();
    let body = if prefixed {
        prefixed_content(skill, "krill-")
    } else {
        skill.content.to_owned()
    };
    files.push((dir.join("SKILL.md"), body));
    for (relative, content) in skill_references(skill.name) {
        let path = dir.join(relative);
        if let Some(parent) = path.parent() {
            ensure_directory_safe(parent)?;
        }
        if !regular_or_missing(&path)? {
            return Err(invalid("skill reference must not be a symlink"));
        }
        match fs::read_to_string(&path) {
            Ok(existing) if !existing.contains(marker_for_path(&path)) => {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "skill reference belongs to another owner",
                ));
            }
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        files.push((path, (*content).to_owned()));
    }
    // Publish references before the entrypoint, so newly discovered instructions have their files.
    let mut changed = false;
    for (path, body) in files.iter().skip(1).chain(files.first()) {
        let marker = marker_for_path(path);
        let bytes = format!("{}\n{marker}\n", body.trim_end());
        changed |= write_if_changed(path, bytes.as_bytes())?;
    }
    if skill_references(skill.name).is_empty() {
        let refs_dir = dir.join("references");
        if ensure_directory_safe(&refs_dir).is_ok()
            && let Ok(entries) = fs::read_dir(&refs_dir)
        {
            for entry in entries.flatten() {
                let p = entry.path();
                if regular_or_missing(&p).unwrap_or(false)
                    && fs::read_to_string(&p).is_ok_and(|s| s.contains(marker_for_path(&p)))
                {
                    fs::remove_file(&p).ok();
                }
            }
            fs::remove_dir(&refs_dir).ok();
        }
    }
    let mut stale = vec![base_dir.join(format!("mk-{}", skill.name))];
    if orca_owned {
        stale.push(base_dir.join(skill.name));
    }
    for old in stale {
        if old != dir && old.join("SKILL.md").is_file() && candidate_writable(&old, skill)? {
            fs::remove_file(old.join("SKILL.md"))?;
            fs::remove_dir(&old).ok();
        }
    }
    Ok((dir.join("SKILL.md"), changed))
}

/// Startup and session launch must never wait for another installer's lifetime.
fn skill_lock(path: &Path) -> io::Result<LockGuard> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    options.mode(0o600);
    let file = options.open(path)?;
    file.try_lock().map_err(|error| match error {
        fs::TryLockError::WouldBlock => io::Error::new(
            io::ErrorKind::WouldBlock,
            "skill synchronization already running; retry on next launch",
        ),
        fs::TryLockError::Error(error) => error,
    })?;
    Ok(LockGuard { _file: file })
}

/// Reject user-controlled directory redirects. Root-level OS aliases such as macOS
/// `/var` -> `/private/var` are outside the provider directory ownership boundary.
pub(super) fn ensure_directory_safe(dir: &Path) -> io::Result<()> {
    // Relative manual install paths need the same ancestor checks as discovery roots.
    let dir = std::path::absolute(dir)?;
    for path in dir
        .ancestors()
        .take_while(|p| p.parent().and_then(Path::parent).is_some())
    {
        match fs::symlink_metadata(path) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(invalid("skill directory must not traverse a symlink"));
            }
            Ok(meta) if !meta.is_dir() => {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "skill parent is not a directory",
                ));
            }
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

pub(super) fn regular_or_missing(path: &Path) -> io::Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(meta) => Ok(meta.is_file() && !meta.file_type().is_symlink()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(true),
        Err(e) => Err(e),
    }
}

fn write_if_changed(path: &Path, bytes: &[u8]) -> io::Result<bool> {
    if fs::read(path).is_ok_and(|existing| existing == bytes) {
        return Ok(false);
    }
    let parent = path
        .parent()
        .ok_or_else(|| invalid("skill file has no parent"))?;
    ensure_directory_safe(parent)?;
    fs::create_dir_all(parent)?;
    let mut temporary = None;
    for _ in 0..16 {
        let seq = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let tmp = parent.join(format!(".krill-skill-{}-{seq}.tmp", std::process::id()));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        options.mode(0o600);
        match options.open(&tmp) {
            Ok(file) => {
                temporary = Some((tmp, file));
                break;
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        }
    }
    let (tmp, mut file) =
        temporary.ok_or_else(|| io::Error::other("skill temporary file collision"))?;
    let result = file.write_all(bytes).and_then(|()| file.sync_all());
    drop(file);
    let result = result.and_then(|()| fs::rename(&tmp, path));
    if result.is_err() {
        fs::remove_file(&tmp).ok();
    }
    result.map(|()| true)
}

#[cfg(test)]
mod tests;

#[cfg(all(test, unix))]
mod sandbox_tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt as _;

    #[test]
    fn up_to_date_readonly_roots_succeed_and_unwritable_missing_roots_skip_quietly() {
        let home = std::env::temp_dir().join(format!("krill-skill-ro-{}", std::process::id()));
        fs::remove_dir_all(&home).ok();
        let populated = home.join("populated");
        let missing_parent = home.join("readonly-parent");
        fs::create_dir_all(&populated).unwrap();
        fs::create_dir_all(&missing_parent).unwrap();
        let initial = sync_builtin_skills(std::slice::from_ref(&populated));
        assert!(initial.failures.is_empty());
        assert_eq!(initial.updated, builtin_skills().len());

        // Remove `.krill-skills.lock` and make both directories read-only (0500) to simulate
        // a sandboxed subshell where `create_dir_all` / lock creation fails with EACCES/EPERM.
        fs::remove_file(populated.join(".krill-skills.lock")).ok();
        fs::set_permissions(&populated, fs::Permissions::from_mode(0o500)).unwrap();
        fs::set_permissions(&missing_parent, fs::Permissions::from_mode(0o500)).unwrap();

        let report = sync_builtin_skills(&[populated.clone(), missing_parent.join("skills")]);
        fs::set_permissions(&populated, fs::Permissions::from_mode(0o700)).ok();
        fs::set_permissions(&missing_parent, fs::Permissions::from_mode(0o700)).ok();
        fs::remove_dir_all(&home).ok();

        assert!(report.failures.is_empty(), "{:?}", report.failures);
        assert_eq!(report.unchanged, builtin_skills().len());
    }
}
