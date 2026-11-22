// Adapted from Orca (Copyright (c) 2026 Lovecast Inc., MIT License).
//! Proposed new-bot state shared between templates, Look Around suggestions, workspace handoff, and `NewBotDialog`.

use serde::{Deserialize, Serialize};

use super::catalog::{BotTemplate, find_bot_template};

/// Maximum number of workspaces a single proposed bot can be created to take over.
pub const HANDED_WORKSPACES_MAX: usize = 20;

/// Maximum characters allowed in the Identity/Purpose box (`BotProposalSeedSchema.purpose`).
pub const BOT_PROPOSAL_PURPOSE_MAX: usize = 200;

/// A workspace a proposed bot is created to take over on its first turn.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HandedWorkspace {
    pub id: String,
    pub name: String,
}

/// Editable fields for a proposed bot before `CreateBotV2` creates it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BotProposal {
    pub name: String,
    pub purpose: String,
    #[serde(default)]
    pub watches: Option<String>,
    #[serde(default)]
    pub does: Option<String>,
    #[serde(default)]
    pub reports: Option<String>,
    #[serde(default)]
    pub automations: Vec<String>,
    #[serde(default)]
    pub template_id: Option<String>,
    #[serde(default)]
    pub mascot: Option<String>,
    #[serde(default)]
    pub worktrees: Vec<HandedWorkspace>,
}

const READ_ONLY_START: &str = "- Change anything (comments, labels, pull requests, posts anywhere but here) until my owner says I can. I start by reading and reporting only.";

const PROPER_FIRST_WORDS: &[&str] = &[
    "GitHub", "Slack", "Linear", "Google", "Apps", "Orca", "Krill",
];

fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn mid_sentence(text: &str) -> String {
    let trimmed = text.trim();
    let first_word = trimmed.split_whitespace().next().unwrap_or("");
    if PROPER_FIRST_WORDS.contains(&first_word) {
        return trimmed.to_owned();
    }
    let mut chars = trimmed.chars();
    let Some(first) = chars.next() else {
        return String::new();
    };
    let mut out = String::new();
    for lower in first.to_lowercase() {
        out.push(lower);
    }
    out.push_str(chars.as_str());
    out
}

fn sentence(text: &str) -> String {
    let trimmed = text.trim();
    if trimmed.ends_with(['.', '!', '?']) {
        trimmed.to_owned()
    } else {
        format!("{trimmed}.")
    }
}

/// Extracts `(watches, does, reports)` from a built-in template's `default_soul` markdown bullets.
#[must_use]
pub fn parse_template_proposal_fields(
    default_soul: &str,
) -> (Option<String>, Option<String>, Option<String>) {
    let mut watches = None;
    let mut does = None;
    let mut reports = None;
    for raw in default_soul.lines() {
        let line = raw.trim();
        if let Some(rest) = line.strip_prefix("- Watches:") {
            let clean = one_line(rest);
            if !clean.is_empty() {
                watches = Some(clean);
            }
        } else if let Some(rest) = line.strip_prefix("- Does:") {
            let clean = one_line(rest);
            if !clean.is_empty() {
                does = Some(clean);
            }
        } else if let Some(rest) = line.strip_prefix("- Reports:") {
            let clean = one_line(rest);
            if !clean.is_empty() {
                reports = Some(clean);
            }
        }
    }
    (watches, does, reports)
}

/// Converts a built-in [`BotTemplate`] into an editable [`BotProposal`].
#[must_use]
pub fn template_proposal(template: &BotTemplate) -> BotProposal {
    let (watches, does, reports) = parse_template_proposal_fields(template.default_soul);
    BotProposal {
        name: template.name.to_owned(),
        purpose: template.description.to_owned(),
        watches,
        does,
        reports,
        automations: Vec::new(),
        template_id: Some(template.id.to_owned()),
        mascot: Some(template.mascot.to_owned()),
        worktrees: Vec::new(),
    }
}

/// Renders `watches`, `does`, and `reports` as the plain sentences shown in `NewBot`'s Identity box.
#[must_use]
pub fn proposal_identity(
    watches: Option<&str>,
    does: Option<&str>,
    reports: Option<&str>,
) -> String {
    let mut parts = Vec::with_capacity(3);
    if let Some(w) = watches.map(one_line).filter(|s| !s.is_empty()) {
        parts.push(sentence(&format!("Watches {}", mid_sentence(&w))));
    }
    if let Some(d) = does.map(one_line).filter(|s| !s.is_empty()) {
        parts.push(sentence(&d));
    }
    if let Some(r) = reports.map(one_line).filter(|s| !s.is_empty()) {
        parts.push(sentence(&format!("Reports {}", mid_sentence(&r))));
    }
    parts.join(" ")
}

/// Builds `SOUL.md` for a newly created bot seeded from a [`BotProposal`].
#[must_use]
pub fn proposal_soul(name: &str, proposal: &BotProposal) -> String {
    let mut how = Vec::new();
    if let Some(w) = proposal
        .watches
        .as_deref()
        .map(one_line)
        .filter(|s| !s.is_empty())
    {
        how.push(format!("- Watches: {w}"));
    }
    if let Some(d) = proposal
        .does
        .as_deref()
        .map(one_line)
        .filter(|s| !s.is_empty())
    {
        how.push(format!("- Does: {d}"));
    }
    if let Some(r) = proposal
        .reports
        .as_deref()
        .map(one_line)
        .filter(|s| !s.is_empty())
    {
        how.push(format!("- Reports: {r}"));
    }
    if !proposal.automations.is_empty() {
        let joined = proposal
            .automations
            .iter()
            .map(|a| one_line(a))
            .filter(|a| !a.is_empty())
            .collect::<Vec<_>>()
            .join("; ");
        if !joined.is_empty() {
            how.push(format!(
                "- Automations to set up once my owner agrees: {joined}"
            ));
        }
    }
    if let Some(tpl_id) = proposal.template_id.as_deref()
        && let Some(tpl) = find_bot_template(tpl_id)
    {
        how.push(format!("- Started from the {} template.", tpl.name));
    }

    let mut out = format!("# {}\n\n{}\n", one_line(name), one_line(&proposal.purpose));
    if !how.is_empty() {
        out.push_str("\n## How I work\n\n");
        for line in how {
            out.push_str(&line);
            out.push('\n');
        }
    }
    out.push_str("\n## Never\n\n");
    out.push_str(READ_ONLY_START);
    out.push('\n');
    out
}

/// First-turn handoff addendum when a proposed bot is created to take over existing workspaces.
#[must_use]
pub fn bot_made_for_workspaces_kickoff(workspaces: &[HandedWorkspace]) -> Option<String> {
    if workspaces.is_empty() {
        return None;
    }
    let list = workspaces
        .iter()
        .take(HANDED_WORKSPACES_MAX)
        .map(|w| format!("{} (id:{})", w.name, w.id))
        .collect::<Vec<_>>()
        .join(", ");
    Some(format!(
        "Your owner made you for these workspaces: {list}.\n\nAfter your owner answers, offer to take them over when they agree."
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bots::BUILTIN_BOT_TEMPLATES;

    #[test]
    fn every_builtin_template_yields_proposal_fields() {
        for tpl in BUILTIN_BOT_TEMPLATES {
            let p = template_proposal(tpl);
            assert!(
                p.watches.as_deref().is_some_and(|s| !s.is_empty()),
                "{} missing watches",
                tpl.id
            );
            assert!(
                p.does.as_deref().is_some_and(|s| !s.is_empty()),
                "{} missing does",
                tpl.id
            );
            assert!(
                p.reports.as_deref().is_some_and(|s| !s.is_empty()),
                "{} missing reports",
                tpl.id
            );
            let identity = proposal_identity(
                p.watches.as_deref(),
                p.does.as_deref(),
                p.reports.as_deref(),
            );
            assert!(
                identity.starts_with("Watches ") && identity.contains("Reports "),
                "{}: {identity}",
                tpl.id
            );
        }
    }

    #[test]
    fn proposal_identity_preserves_connector_names() {
        assert_eq!(
            proposal_identity(
                Some("Linear issues assigned to you"),
                Some("Triage bugs"),
                Some("Here, weekday mornings"),
            ),
            "Watches Linear issues assigned to you. Triage bugs. Reports here, weekday mornings."
        );
        assert_eq!(
            proposal_identity(
                Some("Pull requests waiting on your review."),
                Some("Review each one!"),
                None,
            ),
            "Watches pull requests waiting on your review. Review each one!"
        );
    }

    #[test]
    fn proposal_soul_renders_sections_cleanly() {
        let p = BotProposal {
            name: "Triage".to_owned(),
            purpose: "Keeps relay bugs sorted.".to_owned(),
            watches: Some("New relay issues".to_owned()),
            does: Some("Groups duplicates".to_owned()),
            reports: Some("Here at 9am".to_owned()),
            automations: vec!["Weekdays 9:00: triage".to_owned()],
            template_id: Some("pr-review".to_owned()),
            mascot: Some("B1".to_owned()),
            worktrees: Vec::new(),
        };
        let soul = proposal_soul("Triage", &p);
        assert!(soul.starts_with("# Triage\n\nKeeps relay bugs sorted.\n"));
        assert!(soul.contains("- Watches: New relay issues\n"));
        assert!(soul.contains("- Does: Groups duplicates\n"));
        assert!(soul.contains("- Reports: Here at 9am\n"));
        assert!(
            soul.contains("- Automations to set up once my owner agrees: Weekdays 9:00: triage\n")
        );
        assert!(soul.contains("- Started from the Pull request review template.\n"));
        assert!(soul.contains("## Never\n"));
    }
}
