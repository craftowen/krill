//! Shared hook installation, receipts, conflict checks and item-wise rollback.
//! Callers resolve configuration roots and obtain consent before applying a plan.
//! Paths must have environment-root symlinks resolved; selected files and their
//! descendants still reject symlinks. No process environment is changed here.

mod codex;
mod json;
pub mod store;

use std::io::{self, Write};
use std::path::{Path, PathBuf};

use crate::agent::hooks::{self, Agent};
use serde::{Deserialize, Serialize};

pub use codex::Record as CodexRecord;
use store::error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetAgent {
    Claude,
    Other(Agent),
}

impl TargetAgent {
    pub fn parse(id: &str) -> Option<Self> {
        if id == "claude" {
            Some(Self::Claude)
        } else {
            Agent::parse(id).map(Self::Other)
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Other(a) => a.id(),
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Receipt {
    pub agent: String,
    pub complete: bool,
    pub changes: Vec<Change>,
    pub codex: Option<CodexRecord>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Change {
    pub path: PathBuf,
    pub before: Option<String>,
    pub after: String,
}

#[derive(Debug)]
struct Edit {
    path: PathBuf,
    current: Option<String>,
    next: Option<String>,
}

/// Whether to add hooks or remove only the receipt-owned additions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Install,
    Uninstall,
}

/// A reviewed snapshot. Preparation reads files but never creates locks, backups or receipts.
/// Applying rechecks every snapshot while holding the shared `hooks/install.lock`.
pub struct Plan {
    action: Action,
    receipt_path: PathBuf,
    previous: Option<String>,
    receipt: Receipt,
    edits: Vec<Edit>,
    codex: Option<codex::Plan>,
}

impl std::fmt::Debug for Plan {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Plan")
            .field("action", &self.action)
            .field("receipt", &self.receipt)
            .field("edits", &self.edits)
            .finish_non_exhaustive()
    }
}

impl Plan {
    /// Prepare installation or rollback using explicit config, state and executable paths.
    /// `path` is the selected config file (Codex: `config.toml`, `OpenCode`: plugin file).
    /// `None` means hooks are already recorded for install, or no receipt exists for uninstall.
    /// An incomplete installation is an error until uninstall recovers it.
    pub fn prepare(
        action: Action,
        agent: TargetAgent,
        path: &Path,
        state: &Path,
        exe: &Path,
    ) -> io::Result<Option<Self>> {
        let receipt_path = match agent {
            TargetAgent::Claude => state.join("hooks/claude.json"),
            TargetAgent::Other(a) => hooks::receipt(state, a),
        };
        let previous = store::read(&receipt_path)?;
        if action == Action::Install && previous.is_some() {
            let existing: Receipt = serde_json::from_str(previous.as_deref().unwrap_or(""))?;
            if !existing.complete {
                return Err(error(
                    "interrupted hook installation; uninstall to recover first",
                ));
            }
            return Ok(None);
        }
        let command = match agent {
            TargetAgent::Claude => {
                let exe_str = exe
                    .to_str()
                    .ok_or_else(|| error("krill executable path is not UTF-8"))?;
                #[cfg(windows)]
                {
                    exe_str.to_owned()
                }
                #[cfg(not(windows))]
                {
                    format!(
                        "if [ -n \"${{MK_PANE_ID:-}}\" ]; then exec {} hook claude; else printf '{{}}\\n'; fi",
                        hooks::shell_quote(exe_str)
                    )
                }
            }
            TargetAgent::Other(a) => {
                hooks::command(exe, a).ok_or_else(|| error("krill executable path is not UTF-8"))?
            }
        };
        let mut codex_plan = None;
        let receipt = if action == Action::Install {
            let changes = if agent == TargetAgent::Other(Agent::Codex) {
                let codex_home = path.parent().ok_or_else(|| error("missing Codex home"))?;
                codex_plan = Some(codex::Plan::install(codex_home, &command)?);
                Vec::new()
            } else {
                let path = path.to_path_buf();
                let before = store::read(&path)?;
                let after = match agent {
                    TargetAgent::Other(Agent::OpenCode) => {
                        if before.is_some() {
                            return Err(io::Error::new(
                                io::ErrorKind::AlreadyExists,
                                "OpenCode plugin path already exists; refusing to overwrite",
                            ));
                        }
                        hooks::opencode_plugin(exe)
                            .ok_or_else(|| error("krill executable path is not UTF-8"))?
                    }
                    TargetAgent::Claude => install_claude_json(before.as_deref(), &command)?,
                    TargetAgent::Other(a) => json::install(before.as_deref(), a, &command)?,
                };
                vec![Change {
                    path,
                    before,
                    after,
                }]
            };
            Receipt {
                agent: agent.id().to_owned(),
                complete: false,
                changes,
                codex: codex_plan.as_ref().map(|p| p.record.clone()),
            }
        } else {
            let Some(text) = &previous else {
                return Ok(None);
            };
            let receipt: Receipt = serde_json::from_str(text)?;
            if receipt.agent != agent.id() {
                return Err(error("receipt agent mismatch"));
            }
            if let Some(record) = &receipt.codex {
                codex_plan = Some(codex::Plan::uninstall(record)?);
            }
            receipt
        };
        let mut edits = Vec::new();
        for change in &receipt.changes {
            let current = store::read(&change.path)?;
            let next = if action == Action::Install {
                Some(change.after.clone())
            } else if current == change.before || current.is_none() {
                current.clone()
            } else if current.as_deref() == Some(&change.after) {
                change.before.clone()
            } else if agent == TargetAgent::Other(Agent::OpenCode) {
                eprintln!(
                    "preserved modified OpenCode plugin: {}",
                    change.path.display()
                );
                current.clone()
            } else {
                let text = current
                    .as_deref()
                    .ok_or_else(|| error("configuration disappeared"))?;
                Some(json::uninstall(
                    text,
                    change.before.as_deref(),
                    &change.after,
                )?)
            };
            edits.push(Edit {
                path: change.path.clone(),
                current,
                next,
            });
        }
        Ok(Some(Self {
            action,
            receipt_path,
            previous,
            receipt,
            edits,
            codex: codex_plan,
        }))
    }

    /// Render the same review diff as `krill hook install|uninstall`.
    pub fn write_diff(&self, writer: &mut impl Write) -> io::Result<()> {
        for edit in &self.edits {
            if edit.current == edit.next {
                continue;
            }
            writeln!(
                writer,
                "--- {}\n+++ {}",
                edit.path.display(),
                edit.path.display()
            )?;
            for line in edit.current.as_deref().unwrap_or("").lines() {
                writeln!(writer, "-{line}")?;
            }
            for line in edit.next.as_deref().unwrap_or("").lines() {
                writeln!(writer, "+{line}")?;
            }
        }
        if let Some(plan) = &self.codex {
            plan.diff(writer)?;
        }
        Ok(())
    }

    /// Apply after consent. Failures preserve originals or a pending receipt for uninstall recovery.
    /// Codex trust is still persisted through its version-checked RPC writer.
    pub fn apply(self) -> io::Result<Receipt> {
        let Self {
            action,
            receipt_path,
            previous,
            mut receipt,
            edits,
            codex: mut codex_plan,
        } = self;
        // Hold the receipt lock through all writes. Recheck the reviewed snapshots after consent.
        let _lock = store::lock(
            receipt_path
                .parent()
                .ok_or_else(|| error("missing receipt parent"))?,
        )?;
        if store::read(&receipt_path)? != previous {
            return Err(error("hook receipt changed; rerun to review"));
        }
        for edit in &edits {
            if store::read(&edit.path)? != edit.current {
                return Err(error("configuration changed; rerun to review"));
            }
        }
        if let Some(plan) = &codex_plan
            && store::read(&plan.record.path)? != plan.original
        {
            return Err(error("Codex configuration changed; rerun to review"));
        }
        if action == Action::Install {
            // Write intent first. A crash or partial failure can be recovered by uninstall without
            // guessing ownership, including a Codex writer which committed before losing its reply.
            let text = serde_json::to_string_pretty(&receipt)? + "\n";
            if u64::try_from(text.len()).unwrap_or(u64::MAX) > store::LIMIT {
                return Err(error(
                    "installation receipt exceeds 4 MiB; configuration was not changed",
                ));
            }
            store::replace(&receipt_path, previous.as_deref(), Some(&text))?;
        }
        for edit in edits {
            store::replace(&edit.path, edit.current.as_deref(), edit.next.as_deref())?;
        }
        if let Some(plan) = &mut codex_plan {
            plan.apply()?;
        }
        if action == Action::Install {
            let pending = serde_json::to_string_pretty(&receipt)? + "\n";
            receipt.complete = true;
            let completed = serde_json::to_string_pretty(&receipt)? + "\n";
            store::replace(&receipt_path, Some(&pending), Some(&completed))?;
        }
        if action == Action::Uninstall {
            store::replace(&receipt_path, previous.as_deref(), None)?;
        }
        Ok(receipt)
    }
}

/// Install with consent already obtained by the caller. Returns `None` when already recorded.
/// CLI and GUI use the same state root to share the receipt and installation lock.
/// Parsing, path validation and optimistic-conflict errors never overwrite user settings.
pub fn install_hooks(
    agent: TargetAgent,
    path: &Path,
    state: &Path,
    executable: &Path,
) -> io::Result<Option<Receipt>> {
    Plan::prepare(Action::Install, agent, path, state, executable)?
        .map(Plan::apply)
        .transpose()
}

fn install_claude_json(before: Option<&str>, command: &str) -> io::Result<String> {
    hooks::install_claude_json(before, command)
}

#[cfg(test)]
mod tests;
