// Adapted from Orca (Copyright (c) 2026 Lovecast Inc., MIT License).
//! Built-in Krill Bot catalog: 24 templates across 6 categories, 13 bundled skills,
//! 13 curated MCP apps, 24 curated Composio apps, and local Slack Socket Mode manifest YAML.

use serde::{Deserialize, Serialize};

use crate::orchestration::SkillEntry;

use super::types::{BotMcpAuthKind, ExternalSource};

/// Built-in bot template category (6 categories, 24 templates total).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BotTemplateCategory {
    Coding,
    Releases,
    Support,
    Planning,
    Ops,
    Personal,
}

impl BotTemplateCategory {
    pub const ALL: &'static [Self] = &[
        Self::Coding,
        Self::Releases,
        Self::Support,
        Self::Planning,
        Self::Ops,
        Self::Personal,
    ];

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Coding => "coding",
            Self::Releases => "releases",
            Self::Support => "support",
            Self::Planning => "planning",
            Self::Ops => "ops",
            Self::Personal => "personal",
        }
    }

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Coding => "Coding",
            Self::Releases => "Releases",
            Self::Support => "Support",
            Self::Planning => "Planning",
            Self::Ops => "Ops",
            Self::Personal => "Personal",
        }
    }
}

/// A default cron schedule shipped with a built-in bot template (created disabled by default).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BotTemplateSchedule {
    pub id: &'static str,
    pub when: &'static str,
    pub what: &'static str,
    pub cron: &'static str,
    pub prompt: &'static str,
}

/// An event automation suggestion shipped with a built-in bot template.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BotTemplateEvent {
    pub id: &'static str,
    pub name: &'static str,
    pub source: &'static str,
    pub event: &'static str,
    pub prompt: &'static str,
}

/// A built-in Krill Bot template.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BotTemplate {
    pub id: &'static str,
    pub category: BotTemplateCategory,
    pub name: &'static str,
    pub description: &'static str,
    pub first_chat: &'static str,
    pub mascot: &'static str,
    pub default_soul: &'static str,
    pub connectors: &'static [ExternalSource],
    pub schedules: &'static [BotTemplateSchedule],
    pub events: &'static [BotTemplateEvent],
    pub skills: &'static [&'static str],
    pub mcp_apps: &'static [&'static str],
    pub needs_setup: Option<&'static str>,
    pub example: [&'static str; 2],
}

/// Curated vendor MCP server category.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BotMcpAppCategory {
    Coding,
    Chat,
    Monitoring,
    Docs,
    Mail,
    Sales,
    Design,
}

/// A curated MCP app entry for one-click addition to a bot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BotMcpAppCatalogEntry {
    pub id: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    pub endpoint_url: &'static str,
    pub auth_kind: BotMcpAuthKind,
    pub header_name: Option<&'static str>,
    pub docs_url: &'static str,
    pub category: BotMcpAppCategory,
    pub composio_slugs: &'static [&'static str],
}

/// A curated Composio toolkit app entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ComposioAppCatalogEntry {
    pub slug: &'static str,
    pub name: &'static str,
    pub description: &'static str,
}

/// All 24 built-in Krill Bot templates across 6 categories
/// (`coding`: 6, `releases`: 4, `support`: 4, `planning`: 4, `ops`: 2, `personal`: 4).
pub const BUILTIN_BOT_TEMPLATES: &[BotTemplate] = &[
    // --- Coding (6) ---
    BotTemplate {
        id: "pr-review",
        category: BotTemplateCategory::Coding,
        name: "Pull request review",
        description: "Reviews pull requests you're asked to review and lists risky changes.",
        first_chat: "Asks which repos to cover, then reviews the open pull requests waiting on your review and posts its findings here, with file and line. It doesn't comment on GitHub until you say it can.",
        mascot: "B1",
        default_soul: "# Pull request review\n\nReviews pull requests waiting on your review and lists risky changes and missing tests, with file and line.\n\n## How I work\n\n- Watches: Pull requests waiting on your review, in the repos you pick.\n- Does: Review each one with the `pr-review-checklist` skill and list risky changes and missing tests, with file and line.\n- Reports: Here, when a review is requested.\n\n## Never\n\n- Never post comments, reviews, or approvals on GitHub unless your owner explicitly agrees.\n",
        connectors: &[ExternalSource::GitHub],
        schedules: &[],
        events: &[BotTemplateEvent {
            id: "review-requested",
            name: "When you're asked to review a pull request: review it and post here",
            source: "github",
            event: "review_requested",
            prompt: "A pull request is waiting on my owner's review. Review it with the pr-review-checklist skill and post the findings here, with file and line. Don't comment on GitHub unless my owner said I can.",
        }],
        skills: &["pr-review-checklist"],
        mcp_apps: &[],
        needs_setup: None,
        example: [
            "#4121: the retry counter is never reset, so a second outage stops retries.",
            "No test covers the new timeout path.",
        ],
    },
    BotTemplate {
        id: "bug-investigation",
        category: BotTemplateCategory::Coding,
        name: "Bug investigation",
        description: "Reproduces a reported bug and finds where it starts.",
        first_chat: "Give it a bug report or an issue link. It starts an agent in a new worktree that writes down how to reproduce it, then reports the cause. It writes a fix only after you agree.",
        mascot: "H1",
        default_soul: "# Bug investigation\n\nReproduces reported bugs in a clean worktree, isolates the root cause with file and line, and proposes a fix.\n\n## How I work\n\n- Watches: Bug reports you hand it, and Linear issues assigned to you.\n- Does: Reproduce the bug in a new worktree with the `bug-reproduction` skill, find where it starts, and propose a fix.\n- Reports: Here, when it has a repro and a cause.\n\n## Never\n\n- Never start writing a fix until your owner agrees.\n",
        connectors: &[ExternalSource::GitHub, ExternalSource::Linear],
        schedules: &[],
        events: &[BotTemplateEvent {
            id: "linear-assigned",
            name: "When a Linear issue is assigned to you: investigate it",
            source: "linear",
            event: "assigned",
            prompt: "A Linear issue was assigned to my owner. If it is a bug, reproduce it with the bug-reproduction skill in a new worktree and report the steps and the cause here. Don't start a fix until my owner agrees.",
        }],
        skills: &["bug-reproduction"],
        mcp_apps: &["sentry"],
        needs_setup: None,
        example: [
            "Reproduced KRL-640 on Windows: paste drops the last line when it has no newline.",
            "Cause: the paste end marker goes out before the final chunk. Start a fix?",
        ],
    },
    BotTemplate {
        id: "dependency-updates",
        category: BotTemplateCategory::Coding,
        name: "Dependency updates",
        description: "Checks for outdated and vulnerable packages and proposes upgrades.",
        first_chat: "Lists outdated and vulnerable packages in a repo you pick, most urgent first. After you agree, an agent opens one pull request per group.",
        mascot: "M1",
        default_soul: "# Dependency updates\n\nTracks outdated and vulnerable dependencies and proposes grouped upgrades.\n\n## How I work\n\n- Watches: Packages in the repos you pick.\n- Does: List advisories and outdated packages most urgent first using the `dependency-upgrade` skill; upgrade one group per pull request once agreed.\n- Reports: Here, Mondays at 9 AM.\n\n## Never\n\n- Never open upgrade pull requests without owner approval.\n",
        connectors: &[ExternalSource::GitHub],
        schedules: &[BotTemplateSchedule {
            id: "weekly-check",
            when: "Mondays at 9 AM",
            what: "New advisories and major versions",
            cron: "0 9 * * 1",
            prompt: "Check the repos my owner picked for new security advisories and new major versions since last week. List them here, most urgent first, grouped the way the dependency-upgrade skill says.",
        }],
        events: &[],
        skills: &["dependency-upgrade"],
        mcp_apps: &["context7"],
        needs_setup: None,
        example: [
            "1 advisory: http-client 6.19 has a high-severity fix in 6.21.",
            "4 minor upgrades can go in one pull request.",
        ],
    },
    BotTemplate {
        id: "unreviewed-work",
        category: BotTemplateCategory::Coding,
        name: "Unreviewed agent work",
        description: "A daily list of finished agent work nobody has looked at yet.",
        first_chat: "Lists worktrees whose agent finished with changes you haven't reviewed or turned into a pull request. Krill already tells you when an agent is waiting on you; this covers what's done.",
        mascot: "K2",
        default_soul: "# Unreviewed agent work\n\nKeeps a daily list of finished Krill agent worktrees with changes nobody has reviewed or turned into a pull request yet.\n\n## How I work\n\n- Watches: Your Krill worktrees and sessions.\n- Does: List worktrees whose agent finished with changes that have no pull request and no review yet, with diff size and finish time.\n- Reports: Here, weekdays at 9 AM.\n\n## Never\n\n- Never modify, commit, or delete any worktree on your own.\n",
        connectors: &[],
        schedules: &[BotTemplateSchedule {
            id: "daily-list",
            when: "Weekdays at 9 AM",
            what: "Finished work waiting for review",
            cron: "0 9 * * 1-5",
            prompt: "List my owner's Krill worktrees whose agent finished with changes that have no pull request and no review yet, with the size of each diff and when it finished.",
        }],
        events: &[],
        skills: &[],
        mcp_apps: &[],
        needs_setup: None,
        example: [
            "mobile-pairing finished yesterday with a 340-line diff, no pull request.",
            "2 more finished this week.",
        ],
    },
    BotTemplate {
        id: "workspace-cleanup",
        category: BotTemplateCategory::Coding,
        name: "Workspace cleanup",
        description: "Finds finished or abandoned worktrees and offers to remove them.",
        first_chat: "Lists worktrees whose pull request merged or that haven't changed in two weeks, with any uncommitted changes. It removes only the ones you pick.",
        mascot: "J1",
        default_soul: "# Workspace cleanup\n\nFinds finished or abandoned Krill worktrees and summarizes uncommitted or unpushed state before offering cleanup.\n\n## How I work\n\n- Watches: Your Krill worktrees and their pull requests.\n- Does: List worktrees that are done or abandoned using the `workspace-hygiene` skill, noting anything uncommitted or unpushed.\n- Reports: Here, Fridays at 4 PM.\n\n## Never\n\n- Never remove a worktree unless your owner explicitly names it and confirms.\n",
        connectors: &[ExternalSource::GitHub],
        schedules: &[BotTemplateSchedule {
            id: "weekly-list",
            when: "Fridays at 4 PM",
            what: "Worktrees ready to remove",
            cron: "0 16 * * 5",
            prompt: "List my owner's Krill worktrees that look done or abandoned, using the workspace-hygiene skill, with anything uncommitted or unpushed. Remove nothing until my owner picks.",
        }],
        events: &[],
        skills: &["workspace-hygiene"],
        mcp_apps: &[],
        needs_setup: None,
        example: [
            "6 worktrees are done: their pull requests merged this week.",
            "2 folders haven't changed since Oct 20.",
        ],
    },
    BotTemplate {
        id: "security-check",
        category: BotTemplateCategory::Coding,
        name: "Security check",
        description: "Looks at merged changes for leaked secrets and risky code.",
        first_chat: "Reviews last week's merged pull requests in a repo you pick and lists anything that needs a second look, with file and line.",
        mascot: "N1",
        default_soul: "# Security check\n\nAudits merged changes for leaked secrets, injection vectors, missing auth checks, and unsafe defaults.\n\n## How I work\n\n- Watches: Merged pull requests in the repos you pick.\n- Does: Audit each merged change with the `security-checklist` skill and list concrete findings with file and line.\n- Reports: Here, Mondays at 9 AM and when a pull request merges.\n\n## Never\n\n- Never echo raw leaked secret values into logs or memory; report only file and line and recommend immediate rotation.\n",
        connectors: &[ExternalSource::GitHub],
        schedules: &[BotTemplateSchedule {
            id: "weekly-review",
            when: "Mondays at 9 AM",
            what: "The week's merged changes",
            cron: "0 9 * * 1",
            prompt: "Review last week's merged pull requests in the repos my owner picked with the security-checklist skill. List anything that needs a second look, with file and line, or say nothing was found.",
        }],
        events: &[BotTemplateEvent {
            id: "pr-merged",
            name: "When a pull request merges: check it",
            source: "github",
            event: "pr_merged",
            prompt: "A pull request merged. Check it with the security-checklist skill and post anything that needs a second look here, with file and line. Stay quiet if nothing does.",
        }],
        skills: &["security-checklist"],
        mcp_apps: &[],
        needs_setup: None,
        example: [
            "#4133 logs the full request headers, including the auth token.",
            "Nothing else this week.",
        ],
    },
    // --- Releases (4) ---
    BotTemplate {
        id: "release",
        category: BotTemplateCategory::Releases,
        name: "Release readiness",
        description: "Tracks what's blocking the next release.",
        first_chat: "Keeps a short readiness list: failing checks, pull requests waiting for review, open blockers.",
        mascot: "D1",
        default_soul: "# Release readiness\n\nTracks everything blocking the next release in a short, honest checklist.\n\n## How I work\n\n- Watches: Pull requests, checks, and worktrees.\n- Does: Maintain a readiness list with the `release-readiness` skill: failing checks, PRs awaiting review, open blockers.\n- Reports: Here, weekday mornings.\n\n## Never\n\n- Never tag, publish, or mark a release ready on your own.\n",
        connectors: &[ExternalSource::GitHub],
        schedules: &[BotTemplateSchedule {
            id: "readiness-list",
            when: "Weekdays at 9 AM",
            what: "Readiness list",
            cron: "0 9 * * 1-5",
            prompt: "Update the release readiness list with the release-readiness skill: failing checks, pull requests waiting for review, open blockers. Lead with blocked or ready.",
        }],
        events: &[],
        skills: &["release-readiness"],
        mcp_apps: &[],
        needs_setup: None,
        example: [
            "1.5 is blocked by one failing check: typecheck on #4121.",
            "Two pull requests still need a review: #4108, #4130.",
        ],
    },
    BotTemplate {
        id: "release-notes",
        category: BotTemplateCategory::Releases,
        name: "Release notes",
        description: "Drafts the next release's notes from merged pull requests.",
        first_chat: "Finds the last release tag and drafts notes in three groups: new, fixed, changed. You edit them here.",
        mascot: "E1",
        default_soul: "# Release notes\n\nDrafts clear, user-facing release notes from pull requests merged since the last tag.\n\n## How I work\n\n- Watches: Pull requests merged since the last release tag.\n- Does: Draft notes in three groups (New, Fixed, Changed) using the `release-notes-style` skill.\n- Reports: Here, when asked.\n\n## Never\n\n- Never publish release notes directly; always present a draft for owner review.\n",
        connectors: &[ExternalSource::GitHub],
        schedules: &[],
        events: &[],
        skills: &["release-notes-style"],
        mcp_apps: &[],
        needs_setup: None,
        example: [
            "Draft for 1.5: 4 new, 9 fixed, 2 changed.",
            "Biggest change: bots can export and import sanitized packages.",
        ],
    },
    BotTemplate {
        id: "ci-failures",
        category: BotTemplateCategory::Releases,
        name: "CI failures",
        description: "Explains failed checks and keeps a list of flaky ones.",
        first_chat: "Reads the last two weeks of check runs and lists the flaky ones, then explains each failing check on your open pull requests. It starts a fix only after you agree.",
        mascot: "G1",
        default_soul: "# CI failures\n\nSeparates real CI failures from flaky tests and broken infrastructure, and tracks flaky check frequency.\n\n## How I work\n\n- Watches: Check runs in the repos you pick.\n- Does: Triage failures using the `ci-failure-triage` skill and maintain a ranked list of flaky tests.\n- Reports: Here, Mondays at 9 AM and when a check fails.\n\n## Never\n\n- Never push fixes or disable tests without owner confirmation.\n",
        connectors: &[ExternalSource::GitHub],
        schedules: &[BotTemplateSchedule {
            id: "flaky-list",
            when: "Mondays at 9 AM",
            what: "Flaky checks",
            cron: "0 9 * * 1",
            prompt: "Read the last week of check runs in the repos my owner picked and update the list of flaky checks with the ci-failure-triage skill, worst first.",
        }],
        events: &[BotTemplateEvent {
            id: "check-failed",
            name: "When a check fails: say whether it's flaky or real",
            source: "github",
            event: "ci_failed",
            prompt: "A check failed. If it isn't on a branch my owner cares about, stop without posting. Otherwise sort it with the ci-failure-triage skill and post whether it is real, flaky or infrastructure, with the cause.",
        }],
        skills: &["ci-failure-triage"],
        mcp_apps: &[],
        needs_setup: Some(
            "GitHub polling sees all repository check runs; configure watched repos and target branches in SOUL.md so irrelevant branches stay quiet.",
        ),
        example: [
            "#4130 typecheck: a renamed export, two call sites missed. Fix is one commit.",
            "Flakiest this week: terminal-resize test, 4 of 30 runs.",
        ],
    },
    BotTemplate {
        id: "deploy-watch",
        category: BotTemplateCategory::Releases,
        name: "Deploy watch",
        description: "Posts what changed in each deploy and flags failed ones.",
        first_chat: "Connects to your deployment platform or logs, then summarizes each deploy from the pull requests it shipped and flags failed ones.",
        mascot: "I1",
        default_soul: "# Deploy watch\n\nSummarizes each deploy from the pull requests it shipped and immediately flags failures.\n\n## How I work\n\n- Watches: Deployment events and provider status.\n- Does: Correlate each deploy with its merged pull requests and highlight failures first.\n- Reports: Here, after each deploy.\n\n## Never\n\n- Never trigger rollbacks or production redeploys without explicit owner confirmation.\n",
        connectors: &[ExternalSource::GitHub],
        schedules: &[],
        events: &[BotTemplateEvent {
            id: "deploy-called",
            name: "When a deploy finishes or fails: summarize the deploy",
            source: "webhook",
            event: "call",
            prompt: "A deploy event arrived. Read what it reported, find the pull requests this deploy shipped, and post a short summary here. Lead with a failure if it failed.",
        }],
        skills: &[],
        mcp_apps: &["vercel", "cloudflare", "netlify"],
        needs_setup: Some(
            "Add your deployment MCP app (Vercel, Cloudflare, or Netlify) or forward deploy notifications to this bot.",
        ),
        example: [
            "Production deploy 14:02: 5 pull requests, including the relay retry fix.",
            "Staging deploy failed at migrate: column already exists.",
        ],
    },
    // --- Support (4) ---
    BotTemplate {
        id: "support",
        category: BotTemplateCategory::Support,
        name: "Support triage",
        description: "Keeps a list of questions nobody has answered.",
        first_chat: "Lists unanswered questions in the Slack channels you pick, with links, and notes which already have an issue.",
        mascot: "A1",
        default_soul: "# Support triage\n\nTracks unanswered user questions across support channels and cross-references existing issues.\n\n## How I work\n\n- Watches: Slack channels you pick.\n- Does: List unanswered questions oldest first with links, using the `issue-triage` skill, noting any matching Linear issue.\n- Reports: Here, every 2 hours on weekdays.\n\n## Never\n\n- Never reply directly to customers unless your owner explicitly asks.\n",
        connectors: &[ExternalSource::Slack, ExternalSource::Linear],
        schedules: &[BotTemplateSchedule {
            id: "unanswered",
            when: "Weekdays, every 2 hours from 9 AM to 5 PM",
            what: "Unanswered questions",
            cron: "0 9-17/2 * * 1-5",
            prompt: "List new questions nobody has answered in the Slack channels my owner picked, oldest first, with links. Note which already have an issue.",
        }],
        events: &[],
        skills: &["issue-triage"],
        mcp_apps: &[],
        needs_setup: None,
        example: [
            "3 new questions in #help since 11 AM. Oldest: SSH remote keeps disconnecting, 2 hours.",
            "\"Can I export a bot?\" already has SUP-212.",
        ],
    },
    BotTemplate {
        id: "feedback",
        category: BotTemplateCategory::Support,
        name: "Feedback digest",
        description: "Turns feedback into a weekly list of themes.",
        first_chat: "Groups the last month of feedback in the channels you pick into themes, with counts and quotes.",
        mascot: "F1",
        default_soul: "# Feedback digest\n\nDistills recurring user feedback into concise themes ranked by unique people.\n\n## How I work\n\n- Watches: Slack feedback channels.\n- Does: Group feedback into themes with unique person counts, short quotes, and linked Linear issues using `feedback-themes`.\n- Reports: Here, Mondays at 9 AM.\n\n## Never\n\n- Never invent quotes or merge unrelated complaints into one vague category.\n",
        connectors: &[ExternalSource::Slack, ExternalSource::Linear],
        schedules: &[BotTemplateSchedule {
            id: "weekly-themes",
            when: "Mondays at 9 AM",
            what: "Themes of the week",
            cron: "0 9 * * 1",
            prompt: "Group last week's feedback in the channels my owner picked into themes with the feedback-themes skill, biggest first.",
        }],
        events: &[],
        skills: &["feedback-themes"],
        mcp_apps: &[],
        needs_setup: None,
        example: [
            "Top theme: pasting in the Windows terminal, 7 people, linked KRL-640.",
            "New this week: 3 people asked to export a bot.",
        ],
    },
    BotTemplate {
        id: "bug-intake",
        category: BotTemplateCategory::Support,
        name: "Bug report intake",
        description: "Turns bug reports in Slack into tracked issues with repro steps.",
        first_chat: "Reads the channel you pick and drafts an issue for each new bug report. It files them only after you approve.",
        mascot: "Q1",
        default_soul: "# Bug report intake\n\nConverts Slack bug reports into structured issue drafts with repro steps and duplicate checks.\n\n## How I work\n\n- Watches: The Slack channel where bugs are reported.\n- Does: Draft a Linear issue with repro steps using `issue-triage`; file it once approved.\n- Reports: Here, when a bug is reported.\n\n## Never\n\n- Never create Linear issues before your owner approves the draft.\n",
        connectors: &[ExternalSource::Slack, ExternalSource::Linear],
        schedules: &[],
        events: &[BotTemplateEvent {
            id: "bug-posted",
            name: "When someone posts in your bugs channel: draft an issue",
            source: "slack",
            event: "message",
            prompt: "Someone posted in the bugs channel. If it reports a bug, draft an issue with the issue-triage skill: title, steps, expected and actual result, and any likely duplicate. Post the draft here; file it only after my owner approves.",
        }],
        skills: &["issue-triage"],
        mcp_apps: &[],
        needs_setup: None,
        example: [
            "Drafted: \"Terminal freezes after sleep on macOS\", steps from the thread.",
            "Possible duplicate of KRL-702.",
        ],
    },
    BotTemplate {
        id: "issue-triage",
        category: BotTemplateCategory::Support,
        name: "GitHub issue triage",
        description: "Labels new issues, finds duplicates and asks for missing details.",
        first_chat: "Goes through the last week of new issues in a repo you pick and proposes labels and duplicates here.",
        mascot: "R3",
        default_soul: "# GitHub issue triage\n\nTriages new GitHub issues by proposing labels, flagging duplicates, and listing missing reproduction details.\n\n## How I work\n\n- Watches: New issues in the repos you pick.\n- Does: Propose labels, duplicates, and missing details for each new issue using the `issue-triage` skill.\n- Reports: Here, when an issue is opened.\n\n## Never\n\n- Never edit labels, close issues, or comment on GitHub unless your owner said you can.\n",
        connectors: &[ExternalSource::GitHub],
        schedules: &[],
        events: &[BotTemplateEvent {
            id: "issue-opened",
            name: "When an issue is opened: propose labels and duplicates",
            source: "github",
            event: "issue_opened",
            prompt: "An issue was opened. Triage it with the issue-triage skill and post the proposed labels, likely duplicate and missing details here. Change nothing on GitHub unless my owner said I can.",
        }],
        skills: &["issue-triage"],
        mcp_apps: &[],
        needs_setup: None,
        example: [
            "#2210: bug, windows, duplicate of #2188.",
            "#2214 has no version or steps. Ask for them?",
        ],
    },
    // --- Planning (4) ---
    BotTemplate {
        id: "briefing",
        category: BotTemplateCategory::Planning,
        name: "Project briefing",
        description: "A weekly brief on the projects you pick.",
        first_chat: "Writes what changed, what's blocked and what needs a decision for the projects you pick.",
        mascot: "G1",
        default_soul: "# Project briefing\n\nWrites concise weekly briefs covering what changed, what is blocked, and what needs a decision.\n\n## How I work\n\n- Watches: Linear projects and GitHub repos.\n- Does: Summarize progress in under ten lines using the `status-brief` skill.\n- Reports: Here, Fridays at 3 PM.\n\n## Never\n\n- Never pad briefs with unchanged status items.\n",
        connectors: &[ExternalSource::GitHub, ExternalSource::Linear],
        schedules: &[BotTemplateSchedule {
            id: "weekly-brief",
            when: "Fridays at 3 PM",
            what: "The brief",
            cron: "0 15 * * 5",
            prompt: "Write this week's brief on the projects my owner picked with the status-brief skill: changed, blocked, needs a decision.",
        }],
        events: &[],
        skills: &["status-brief"],
        mcp_apps: &[],
        needs_setup: None,
        example: [
            "Mobile v2: 6 issues closed, 2 waiting on design.",
            "Needs a decision: KRL-902, push permission copy.",
        ],
    },
    BotTemplate {
        id: "docs-upkeep",
        category: BotTemplateCategory::Planning,
        name: "Docs upkeep",
        description: "Flags docs that no longer match merged code.",
        first_chat: "Reads last week's merged pull requests and lists docs pages that now say something wrong.",
        mascot: "L2",
        default_soul: "# Docs upkeep\n\nDetects documentation drift caused by recently merged pull requests.\n\n## How I work\n\n- Watches: Merged pull requests and documentation files in the repos you pick.\n- Does: Identify docs pages that now contradict merged behavior, citing page, line, and what changed.\n- Reports: Here, Wednesdays at 10 AM.\n\n## Never\n\n- Never rewrite docs without owner confirmation.\n",
        connectors: &[ExternalSource::GitHub],
        schedules: &[BotTemplateSchedule {
            id: "weekly-docs",
            when: "Wednesdays at 10 AM",
            what: "Docs to update",
            cron: "0 10 * * 3",
            prompt: "Read last week's merged pull requests in the repos my owner picked and list docs pages that now say something wrong, with the page, the line and what changed.",
        }],
        events: &[],
        skills: &[],
        mcp_apps: &["notion", "atlassian"],
        needs_setup: None,
        example: [
            "docs/remote.md still says SSH needs a restart; #4108 removed that.",
            "1 more page mentions the old setting name.",
        ],
    },
    BotTemplate {
        id: "spec-drafts",
        category: BotTemplateCategory::Planning,
        name: "Spec drafting",
        description: "Turns a discussion into a short spec you can share.",
        first_chat: "Give it a Slack thread or a doc. It drafts a one-page spec and asks about anything left open.",
        mascot: "T1",
        default_soul: "# Spec drafting\n\nTurns messy threads and notes into sharp one-page specs ready to share.\n\n## How I work\n\n- Watches: Slack threads and docs you hand it.\n- Does: Draft a one-page spec (Problem, Decision, Scope, Open questions) with the `spec-format` skill.\n- Reports: Here, when asked.\n\n## Never\n\n- Never present inferred assumptions as decided facts.\n",
        connectors: &[ExternalSource::Slack, ExternalSource::Google],
        schedules: &[],
        events: &[],
        skills: &["spec-format"],
        mcp_apps: &["notion"],
        needs_setup: None,
        example: [
            "Draft: \"Bot sharing v2\". 3 decisions, 2 open questions.",
            "Saved to your Drive as a doc.",
        ],
    },
    BotTemplate {
        id: "backlog-cleanup",
        category: BotTemplateCategory::Planning,
        name: "Backlog cleanup",
        description: "Finds stale, duplicate and unowned issues.",
        first_chat: "Lists issues untouched for 60 days, likely duplicates and issues with no owner, for a team you pick.",
        mascot: "P3",
        default_soul: "# Backlog cleanup\n\nSurfaces stale, duplicate, and unowned issues once a month so backlogs stay actionable.\n\n## How I work\n\n- Watches: Linear issues for the team you pick.\n- Does: List issues untouched for 60+ days, likely duplicates using `issue-triage`, and unowned issues.\n- Reports: Here, on the 1st of each month.\n\n## Never\n\n- Never close, reassign, or archive issues until your owner approves.\n",
        connectors: &[ExternalSource::Linear],
        schedules: &[BotTemplateSchedule {
            id: "monthly-list",
            when: "1st of each month at 9 AM",
            what: "Cleanup list",
            cron: "0 9 1 * *",
            prompt: "List the team's Linear issues untouched for 60 days, likely duplicates (using the issue-triage skill) and issues with no owner. Change nothing until my owner says so.",
        }],
        events: &[],
        skills: &["issue-triage"],
        mcp_apps: &[],
        needs_setup: None,
        example: [
            "14 issues untouched since August.",
            "KRL-511 and KRL-688 look like the same bug.",
        ],
    },
    // --- Ops (2) ---
    BotTemplate {
        id: "incident-notes",
        category: BotTemplateCategory::Ops,
        name: "Incident notes",
        description: "Keeps a timeline in an incident channel and drafts the write-up.",
        first_chat: "Follows the incident channel you pick and its threads, keeps a timestamped timeline, and drafts a blameless write-up when it's over.",
        mascot: "H2",
        default_soul: "# Incident notes\n\nMaintains a factual, timestamped incident timeline and drafts a blameless postmortem when resolved.\n\n## How I work\n\n- Watches: The incident channel you pick and its threads.\n- Does: Record key events and draft a blameless write-up using the `incident-timeline` skill.\n- Reports: Here, during and after an incident.\n\n## Never\n\n- Never blame individuals; describe systems, timelines, and decisions factually.\n",
        connectors: &[ExternalSource::Slack],
        schedules: &[],
        events: &[BotTemplateEvent {
            id: "incident-posted",
            name: "When someone posts in an incident channel: add it to the timeline",
            source: "slack",
            event: "message",
            prompt: "Someone posted in the incident channel. Add what matters to the timeline with the incident-timeline skill, and read the thread it starts too. Post only when the timeline changes.",
        }],
        skills: &["incident-timeline"],
        mcp_apps: &["datadog", "sentry"],
        needs_setup: None,
        example: [
            "14:02 alert fired; 14:09 rollback started; 14:21 errors back to normal.",
            "Write-up draft ready.",
        ],
    },
    BotTemplate {
        id: "oncall-handoff",
        category: BotTemplateCategory::Ops,
        name: "On-call handoff",
        description: "A weekly summary of alerts, incidents and open follow-ups.",
        first_chat: "Reads your alert channel and incident issues from the past week and writes the handoff.",
        mascot: "B1",
        default_soul: "# On-call handoff\n\nCompiles weekly alert counts, incident summaries, and open follow-ups for smooth on-call rotation.\n\n## How I work\n\n- Watches: Your alert channel and incident issues.\n- Does: Summarize the week's alerts, incidents, and open follow-ups with owners using `incident-timeline`.\n- Reports: Here, Fridays at 4 PM.\n\n## Never\n\n- Never close incident follow-up tickets on your own.\n",
        connectors: &[ExternalSource::Slack, ExternalSource::Linear],
        schedules: &[BotTemplateSchedule {
            id: "weekly-handoff",
            when: "Fridays at 4 PM",
            what: "Handoff",
            cron: "0 16 * * 5",
            prompt: "Write the on-call handoff for the past week: alerts by count, incidents with a one-line summary each, and open follow-ups with owners.",
        }],
        events: &[],
        skills: &["incident-timeline"],
        mcp_apps: &["datadog", "sentry"],
        needs_setup: Some(
            "Reads alerts posted in Slack or connected monitoring MCP tools (Datadog/Sentry).",
        ),
        example: [
            "12 alerts, 2 incidents. Disk alert on cell-3 fired 5 times.",
            "3 follow-ups still open.",
        ],
    },
    // --- Personal (4) ---
    BotTemplate {
        id: "followup",
        category: BotTemplateCategory::Personal,
        name: "Replies you owe",
        description: "Reminds you of replies you still owe.",
        first_chat: "Finds questions to you with no reply, and things you said you'd do, in Slack threads and issue comments.",
        mascot: "E1",
        default_soul: "# Replies you owe\n\nCatches unanswered questions addressed to you and commitments you made in threads and comments.\n\n## How I work\n\n- Watches: Slack threads and Linear comments.\n- Does: Surface unanswered questions and open commitments oldest first, with links.\n- Reports: Here, daily at 4 PM.\n\n## Never\n\n- Never send replies on your owner's behalf without asking.\n",
        connectors: &[ExternalSource::Slack, ExternalSource::Linear],
        schedules: &[BotTemplateSchedule {
            id: "daily-owed",
            when: "Daily at 4 PM",
            what: "What you still owe",
            cron: "0 16 * * *",
            prompt: "Find questions to my owner with no reply, and things my owner said they'd do, in Slack threads and Linear comments. Oldest first, with links.",
        }],
        events: &[],
        skills: &[],
        mcp_apps: &[],
        needs_setup: None,
        example: [
            "You told a teammate \"I'll check tomorrow\" in #design, 2 days ago.",
            "KRL-881 has a question for you from yesterday.",
        ],
    },
    BotTemplate {
        id: "standup",
        category: BotTemplateCategory::Personal,
        name: "Daily standup notes",
        description: "What you and your agents did yesterday, and what's next.",
        first_chat: "Writes yesterday's notes from your worktrees, merged pull requests and moved issues. You copy them where your team reads them.",
        mascot: "O1",
        default_soul: "# Daily standup notes\n\nDrafts brief daily standup notes from your Krill worktrees, merged pull requests, and moved issues.\n\n## How I work\n\n- Watches: Your Krill worktrees, pull requests, and Linear issues.\n- Does: Summarize yesterday's completed work and today's plan using the `status-brief` skill.\n- Reports: Here, weekdays at 9 AM.\n\n## Never\n\n- Never post standup notes to a public channel unless asked.\n",
        connectors: &[ExternalSource::GitHub, ExternalSource::Linear],
        schedules: &[BotTemplateSchedule {
            id: "daily-notes",
            when: "Weekdays at 9 AM",
            what: "Standup notes",
            cron: "0 9 * * 1-5",
            prompt: "Write my owner's standup notes with the status-brief skill: what they and their agents did yesterday (worktrees, merged pull requests, moved issues) and what is next.",
        }],
        events: &[],
        skills: &["status-brief"],
        mcp_apps: &[],
        needs_setup: None,
        example: [
            "Yesterday: merged #4121, relay retry. Agents finished 2 worktrees.",
            "Today: KRL-702, review #4130.",
        ],
    },
    BotTemplate {
        id: "email-digest",
        category: BotTemplateCategory::Personal,
        name: "Email digest",
        description: "Twice a day, the emails that need a reply from you.",
        first_chat: "Reads today's inbox and lists emails that need your reply, newest first. It never sends email.",
        mascot: "I1",
        default_soul: "# Email digest\n\nScans your inbox twice on weekdays and surfaces only emails that need a human reply.\n\n## How I work\n\n- Watches: Your Gmail inbox.\n- Does: List emails needing a reply newest first, one line each, skipping newsletters and automated alerts.\n- Reports: Here, weekdays at 9 AM and 2 PM.\n\n## Never\n\n- Never send, archive, or delete emails.\n",
        connectors: &[ExternalSource::Google],
        schedules: &[
            BotTemplateSchedule {
                id: "morning",
                when: "Weekdays at 9 AM",
                what: "Emails that need you",
                cron: "0 9 * * 1-5",
                prompt: "List emails since yesterday afternoon that need a reply from my owner, newest first, one line each. Skip newsletters and notifications. Never send email.",
            },
            BotTemplateSchedule {
                id: "afternoon",
                when: "Weekdays at 2 PM",
                what: "Emails that need you",
                cron: "0 14 * * 1-5",
                prompt: "List emails since this morning that need a reply from my owner, newest first, one line each. Skip newsletters and notifications. Never send email.",
            },
        ],
        events: &[],
        skills: &[],
        mcp_apps: &[],
        needs_setup: None,
        example: [
            "2 need a reply: the security review date, and an invoice question.",
            "14 newsletters skipped.",
        ],
    },
    BotTemplate {
        id: "meeting-prep",
        category: BotTemplateCategory::Personal,
        name: "Meeting prep",
        description: "Before each meeting, the issues, pull requests and docs it's about.",
        first_chat: "Looks at today's calendar and writes a short prep note for each meeting.",
        mascot: "S2",
        default_soul: "# Meeting prep\n\nPrepares a short context briefing before each meeting with relevant issues, pull requests, and docs.\n\n## How I work\n\n- Watches: Your Google Calendar, GitHub, and Linear.\n- Does: 15 minutes before each meeting, list attendees and linked issues, pull requests, and docs.\n- Reports: Here, before each meeting.\n\n## Never\n\n- Never edit calendar invites or send messages to attendees.\n",
        connectors: &[
            ExternalSource::Google,
            ExternalSource::GitHub,
            ExternalSource::Linear,
        ],
        schedules: &[],
        events: &[BotTemplateEvent {
            id: "meeting-soon",
            name: "15 minutes before a meeting: prep note",
            source: "calendar",
            event: "starting_soon",
            prompt: "A meeting starts in 15 minutes. Write a short prep note: who is in it, and the issues, pull requests and docs it is about, with links.",
        }],
        skills: &[],
        mcp_apps: &[],
        needs_setup: None,
        example: [
            "Mobile sync at 11: KRL-902 needs a decision; #4130 is waiting on you.",
            "Doc: \"Push permission copy\".",
        ],
    },
];

/// All 13 bundled Krill Bot skills adapted from Orca (`orca` -> `krill`).
pub const BUILTIN_BOT_SKILLS: &[SkillEntry] = &[
    SkillEntry {
        name: "pr-review-checklist",
        description: "How to review a pull request diff and report risky changes, each with file and line.",
        content: "---\nname: pr-review-checklist\ndescription: How to review a pull request diff and report risky changes, each with file and line.\n---\n\n# Reviewing a pull request\n\nRead the whole diff before writing anything. Then read the code around each change that matters: callers, tests, and the files it touches indirectly.\n\n## What to look for\n\n1. **Behaviour changes.** Anything a user or another module would notice: a new default, a removed branch, a different error, changed ordering, a renamed setting.\n2. **Edges.** Empty input, a missing value, a second call, a retry, a timeout, concurrent callers, a very large input.\n3. **Missing tests.** A new branch or a fixed bug with no test that would fail without the change.\n4. **Risky edits.** Migrations, deletes, permission checks, anything that runs on startup, shared state, public APIs and wire formats.\n5. **Leftovers.** Debug logging, commented-out code, TODOs that hide a real gap.\n\n## How to report\n\n- One finding per bullet, most serious first.\n- Each finding names the file and line (`src/retry.rs:42`) and says what goes wrong, for whom, and when.\n- Say how sure you are when you are not sure. Don't pad the list: three real findings beat ten guesses.\n- Style and naming only when they hide a bug.\n- End with one line: ready to merge, or what must change first.\n\n## Limits\n\n- Report here, in chat. Comment on the pull request only when your owner said you can.\n- Never push to someone else's branch.\n",
    },
    SkillEntry {
        name: "bug-reproduction",
        description: "Reproduce a reported bug with exact steps before looking for its cause, then report the cause.",
        content: "---\nname: bug-reproduction\ndescription: Reproduce a reported bug with exact steps before looking for its cause, then report the cause.\n---\n\n# Reproducing a bug\n\nA bug you can't make happen on purpose isn't understood yet. Reproduce first, explain second, fix last.\n\n## Steps\n\n1. **Collect the facts.** Version, platform, what the reporter did, what they expected, what happened. Note what is missing and ask for it once.\n2. **Write the steps down** as a numbered list someone else could follow, starting from a clean state.\n3. **Make it happen.** Run the steps. If it doesn't happen, change one thing at a time (platform, data size, timing, settings) and record each try.\n4. **Make it small.** Remove steps until removing any more makes the bug go away.\n5. **Prove it with a test** when the code has tests: a failing test is the best repro.\n6. **Find where it starts.** Follow the data from the last correct point to the first wrong one. Name the file and line.\n\n## Report\n\n- The steps, whether they reproduce it every time or sometimes (and how often).\n- The cause in one or two sentences, with file and line.\n- A suggested fix and its risk. Start the fix only after your owner agrees.\n\nIf you can't reproduce it, say what you tried and what would help.\n",
    },
    SkillEntry {
        name: "ci-failure-triage",
        description: "Tell a flaky check from a real failure or broken infrastructure, and find the change that started it.",
        content: "---\nname: ci-failure-triage\ndescription: Tell a flaky check from a real failure or broken infrastructure, and find the change that started it.\n---\n\n# Triaging a failed check\n\n## Sort it first\n\n- **Real failure:** the same test fails the same way on retry, and the failure points at code in the change.\n- **Flaky:** it passes on retry with no code change, or fails on unrelated changes too. Look at the last runs of the same check on other branches.\n- **Infrastructure:** the runner, network, cache, package registry or a timeout before any test ran. The log ends early or names a service, not your code.\n\n## For a real failure\n\n1. Read the first error, not the last. Later errors are often its echo.\n2. Find the commit that started it: the first failing run after a passing one on the same branch.\n3. Name the file and line and say why the change broke it.\n\n## For a flaky check\n\n- Keep a list: check name, test name, how many of the last runs failed, the error line.\n- Flag a test that fails more than about one run in ten.\n\n## Report\n\nOne line per check: name, kind (real, flaky, infrastructure), the cause or the evidence, and the next step. Start a fix only after your owner agrees.\n",
    },
    SkillEntry {
        name: "release-readiness",
        description: "What blocks a release and how to list it, from failing checks to open blockers.",
        content: "---\nname: release-readiness\ndescription: What blocks a release and how to list it, from failing checks to open blockers.\n---\n\n# Release readiness\n\nA release is ready when nothing on this list is open. Keep the list short and current.\n\n## What blocks a release\n\n1. **Failing required checks** on the release branch or on pull requests marked for the release.\n2. **Pull requests waiting for review** that the release needs.\n3. **Open blocker issues:** labelled blocker, or assigned to the release milestone and not closed.\n4. **Unmerged fixes** for a regression found since the last release.\n\n## How to list it\n\n- Group by the four kinds above, with links.\n- For each item: who owns it and how long it has been waiting.\n- First line: blocked or ready, and the single biggest blocker.\n- Leave out anything that doesn't block. Nice-to-haves go in a separate \"after the release\" line, if at all.\n\n## Keep it honest\n\n- Re-check each item before reporting it; a merged pull request or a green re-run is no longer a blocker.\n- Don't mark the release ready on your own. Say it looks ready and let your owner decide.\n",
    },
    SkillEntry {
        name: "release-notes-style",
        description: "Write release notes for users in three groups, new, fixed and changed, without internal detail.",
        content: "---\nname: release-notes-style\ndescription: Write release notes for users in three groups, new, fixed and changed, without internal detail.\n---\n\n# Release notes\n\nNotes are for the people who use the product, not for the people who built it.\n\n## Gather\n\n- Find the last release tag and list every pull request merged since then.\n- Skip refactors, test-only changes, dependency bumps with no user effect, and internal tooling.\n\n## Write\n\nThree groups, in this order, each a short bulleted list:\n\n- **New:** things users can do now that they couldn't before.\n- **Fixed:** problems users could hit that are gone. Say what used to go wrong, not how it was fixed.\n- **Changed:** things that work differently now, including anything removed.\n\nEach bullet is one sentence in plain words. Lead with what the user sees. Leave out code names, file names and internal project names. Link the pull request at the end only if your owner wants links.\n\n## Before sharing\n\n- Put the biggest change first in its group.\n- Ask your owner about anything you couldn't tell was user-facing.\n- Post the draft here for editing; never publish it yourself.\n",
    },
    SkillEntry {
        name: "dependency-upgrade",
        description: "Upgrade dependencies one group at a time, reading changelogs and running tests, one pull request each.",
        content: "---\nname: dependency-upgrade\ndescription: Upgrade dependencies one group at a time, reading changelogs and running tests, one pull request each.\n---\n\n# Upgrading dependencies\n\n## Find what to upgrade\n\n- Security advisories first, by severity.\n- Then major versions, then minor and patch updates.\n- Use the project's own package manager and its audit command; don't switch tools.\n\n## Group them\n\n- Packages released together (a framework and its plugins) go in one group.\n- Each major version upgrade is its own group.\n- Minor and patch updates with no advisories can share one group.\n\n## For each group\n\n1. Read the changelog or release notes between the current and the new version. Note breaking changes and anything deprecated that the code uses.\n2. Upgrade only that group. Keep the lockfile change in the same commit.\n3. Run the build and the tests. Fix what the upgrade broke, or stop and report it.\n4. Open one pull request per group. Its description lists the versions, the breaking changes that applied, and what you changed because of them.\n\n## Report\n\nMost urgent first: package, current and new version, why (advisory or major), and the effort you expect. Open pull requests only after your owner agrees.\n",
    },
    SkillEntry {
        name: "security-checklist",
        description: "What to look for in a merged change for leaked secrets, injection, missing auth checks and unsafe defaults.",
        content: "---\nname: security-checklist\ndescription: What to look for in a merged change for leaked secrets, injection, missing auth checks and unsafe defaults.\n---\n\n# Security check of a change\n\nRead each merged change for the problems below. Report only what you can point to.\n\n## Secrets\n\n- Keys, tokens, passwords or private URLs in code, config, tests or fixtures.\n- Logs or error messages that print headers, tokens, cookies or full request bodies.\n\n## Injection\n\n- User input reaching a shell command, SQL, a file path, HTML or a template without escaping or a fixed argument list.\n- Paths built from input that could leave the intended folder (`..`).\n\n## Access\n\n- A new endpoint, command or route with no permission check, or a check that runs after the work.\n- A check that trusts something the caller controls (a header, an ID in the body).\n\n## Unsafe defaults\n\n- Features or debug modes turned on by default, wide CORS, disabled certificate checks, world-readable files.\n- New dependencies from unknown publishers, or install scripts.\n\n## Report\n\n- One finding per bullet with file and line, what an attacker could do, and how likely it is.\n- If a secret leaked, say so first: it needs rotating, not just deleting.\n- Say \"nothing found\" when nothing is found; don't stretch.\n",
    },
    SkillEntry {
        name: "issue-triage",
        description: "Triage an issue or report, labels, duplicates and missing details, asking for what is missing in one comment.",
        content: "---\nname: issue-triage\ndescription: Triage an issue or report, labels, duplicates and missing details, asking for what is missing in one comment.\n---\n\n# Triaging an issue\n\n## For each new issue or report\n\n1. **Read it all**, including screenshots and logs.\n2. **Kind:** bug, feature request, question, or support. Use the labels the project already has; never invent new ones.\n3. **Area:** the part of the product it is about, if the project labels areas.\n4. **Duplicates:** search open and recently closed issues for the same symptom. Link the best match and say why it matches.\n5. **Missing details:** for a bug, check for version, platform, steps, expected and actual result. List what is missing.\n\n## Asking for details\n\nAsk once, in one comment, for everything missing. Be specific (\"Which version? Settings -> About shows it.\") and polite. Don't ask for what is already there.\n\n## Report\n\nOne line per issue: link, proposed kind and labels, likely duplicate (if any), and what is missing. Apply labels, close duplicates or post comments only when your owner said you can.\n",
    },
    SkillEntry {
        name: "feedback-themes",
        description: "Group feedback into themes with counts and short quotes, linking issues that already exist.",
        content: "---\nname: feedback-themes\ndescription: Group feedback into themes with counts and short quotes, linking issues that already exist.\n---\n\n# Feedback themes\n\n## Collect\n\n- Read every message in the period and channels you were given. Count people, not messages: one person repeating themselves counts once.\n- Skip thanks, chatter and messages you can't tie to the product.\n\n## Group\n\n- A theme is one problem or wish, named in the user's words (\"pasting in the terminal drops lines\"), not a product area (\"terminal\").\n- Put each message in at most one theme. Leave out themes with a single person unless they are severe.\n\n## For each theme\n\n- How many people, and whether that grew since last time.\n- One or two short quotes, as written, with a link.\n- The existing issue it matches, if there is one; otherwise say there's none.\n\n## Report\n\nBiggest theme first. Then a short \"new this time\" line for themes that weren't there before. Keep it to what fits on one screen.\n",
    },
    SkillEntry {
        name: "status-brief",
        description: "Write a short status brief, what changed, what is blocked, what needs a decision, in under ten lines.",
        content: "---\nname: status-brief\ndescription: Write a short status brief, what changed, what is blocked, what needs a decision, in under ten lines.\n---\n\n# Status brief\n\nA brief is read in under a minute. Under ten lines, always.\n\n## Three parts\n\n1. **Changed:** what moved since the last brief. Merged pull requests, closed issues, finished work. Group small items (\"6 issues closed\").\n2. **Blocked:** what can't move, and on whom or what. Include how long it has been blocked.\n3. **Needs a decision:** questions only a person can answer, with a link to where the question is.\n\n## Rules\n\n- Lead with the part that needs action, if there is one.\n- Links instead of descriptions. Numbers instead of adjectives.\n- Leave out anything unchanged since the last brief.\n- If nothing changed, say so in one line.\n",
    },
    SkillEntry {
        name: "spec-format",
        description: "Turn a discussion into a one-page spec, problem, decision, scope and open questions.",
        content: "---\nname: spec-format\ndescription: Turn a discussion into a one-page spec, problem, decision, scope and open questions.\n---\n\n# One-page spec\n\n## Sections\n\n1. **Problem:** who has it, when, and what it costs them. Two or three sentences.\n2. **Decision:** what was agreed, stated plainly. If nothing was agreed, say so and list the options.\n3. **Scope:** what is in, and what is explicitly out.\n4. **Open questions:** each one with who can answer it.\n\n## Rules\n\n- One page. Cut background that doesn't change the decision.\n- Use the discussion's own facts; mark anything you inferred.\n- Link the source thread or doc at the top.\n- Ask your owner about every open question before calling the spec done.\n",
    },
    SkillEntry {
        name: "incident-timeline",
        description: "Keep a timestamped incident timeline, then draft a blameless write-up when the incident is over.",
        content: "---\nname: incident-timeline\ndescription: Keep a timestamped incident timeline, then draft a blameless write-up when the incident is over.\n---\n\n# Incident timeline\n\n## During the incident\n\n- Add an entry for each event that matters: alert fired, someone joined, a cause was suspected or ruled out, a change was made, impact started or ended.\n- Each entry: time (with time zone), what happened, link to the message or graph.\n- Write facts, not judgements. Note who did something only when it matters for the timeline.\n\n## When it is over\n\nDraft the write-up:\n\n1. **Summary:** what broke, for whom, and for how long.\n2. **Timeline:** the entries above, trimmed.\n3. **Cause:** what made it happen, and what let it reach users.\n4. **What went well and what didn't.**\n5. **Follow-ups:** each with an owner to be agreed.\n\nBlameless: describe systems and decisions, never fault people. Post the draft here for your owner to edit.\n",
    },
    SkillEntry {
        name: "workspace-hygiene",
        description: "When a Krill worktree is done or abandoned, and what to check before removing it.",
        content: "---\nname: workspace-hygiene\ndescription: When a Krill worktree is done or abandoned, and what to check before removing it.\n---\n\n# Workspace hygiene\n\n## Done or abandoned\n\n- **Done:** its pull request merged or closed, and nothing new since.\n- **Abandoned:** no changes for two weeks or more and no open pull request.\n- **Folder workspaces** have no branch or pull request: judge them by recent activity only.\n\n## Before removing one, check\n\n1. Uncommitted changes. List the files.\n2. Commits that were never pushed.\n3. A running agent or terminal in it.\n\nNever remove a worktree with uncommitted or unpushed work unless your owner names it and says so.\n\n## Report\n\nA list: worktree, why it looks done or abandoned, and anything from the checks above. Remove only the ones your owner picks, with `krill worktree rm`.\n",
    },
];

/// All 13 curated vendor MCP apps adapted from Orca.
pub const BOT_MCP_APPS: &[BotMcpAppCatalogEntry] = &[
    BotMcpAppCatalogEntry {
        id: "vercel",
        name: "Vercel",
        description: "Check deployments and read their logs",
        endpoint_url: "https://mcp.vercel.com",
        auth_kind: BotMcpAuthKind::BrowserOAuth,
        header_name: None,
        docs_url: "https://vercel.com/docs/mcp",
        category: BotMcpAppCategory::Coding,
        composio_slugs: &[],
    },
    BotMcpAppCatalogEntry {
        id: "sentry",
        name: "Sentry",
        description: "Look into errors and the events behind them",
        endpoint_url: "https://mcp.sentry.dev/mcp",
        auth_kind: BotMcpAuthKind::BrowserOAuth,
        header_name: None,
        docs_url: "https://docs.sentry.io/product/sentry-mcp/",
        category: BotMcpAppCategory::Monitoring,
        composio_slugs: &["sentry"],
    },
    BotMcpAppCatalogEntry {
        id: "datadog",
        name: "Datadog",
        description: "Search logs, metrics and monitors",
        endpoint_url: "https://mcp.datadoghq.com/v1/mcp",
        auth_kind: BotMcpAuthKind::BrowserOAuth,
        header_name: None,
        docs_url: "https://docs.datadoghq.com/bits_ai/mcp_server/",
        category: BotMcpAppCategory::Monitoring,
        composio_slugs: &[],
    },
    BotMcpAppCatalogEntry {
        id: "posthog",
        name: "PostHog",
        description: "Query product analytics and feature flags",
        endpoint_url: "https://mcp.posthog.com/mcp",
        auth_kind: BotMcpAuthKind::BrowserOAuth,
        header_name: None,
        docs_url: "https://posthog.com/docs/model-context-protocol",
        category: BotMcpAppCategory::Monitoring,
        composio_slugs: &[],
    },
    BotMcpAppCatalogEntry {
        id: "cloudflare",
        name: "Cloudflare",
        description: "Manage Workers, DNS and the rest of an account",
        endpoint_url: "https://mcp.cloudflare.com/mcp",
        auth_kind: BotMcpAuthKind::BrowserOAuth,
        header_name: None,
        docs_url: "https://developers.cloudflare.com/agents/model-context-protocol/",
        category: BotMcpAppCategory::Coding,
        composio_slugs: &[],
    },
    BotMcpAppCatalogEntry {
        id: "supabase",
        name: "Supabase",
        description: "Query databases and manage projects",
        endpoint_url: "https://mcp.supabase.com/mcp",
        auth_kind: BotMcpAuthKind::BrowserOAuth,
        header_name: None,
        docs_url: "https://supabase.com/docs/guides/getting-started/mcp",
        category: BotMcpAppCategory::Coding,
        composio_slugs: &[],
    },
    BotMcpAppCatalogEntry {
        id: "neon",
        name: "Neon",
        description: "Run SQL and manage Postgres branches",
        endpoint_url: "https://mcp.neon.tech/mcp",
        auth_kind: BotMcpAuthKind::BrowserOAuth,
        header_name: None,
        docs_url: "https://neon.tech/docs/ai/neon-mcp-server",
        category: BotMcpAppCategory::Coding,
        composio_slugs: &[],
    },
    BotMcpAppCatalogEntry {
        id: "netlify",
        name: "Netlify",
        description: "Deploy sites and check their builds",
        endpoint_url: "https://mcp.netlify.com/mcp",
        auth_kind: BotMcpAuthKind::BrowserOAuth,
        header_name: None,
        docs_url: "https://docs.netlify.com/welcome/build-with-ai/netlify-mcp-server/",
        category: BotMcpAppCategory::Coding,
        composio_slugs: &[],
    },
    BotMcpAppCatalogEntry {
        id: "gitlab",
        name: "GitLab",
        description: "Read and update issues and merge requests",
        endpoint_url: "https://gitlab.com/api/v4/mcp",
        auth_kind: BotMcpAuthKind::BrowserOAuth,
        header_name: Some("PRIVATE-TOKEN"),
        docs_url: "https://docs.gitlab.com/user/gitlab_duo/model_context_protocol/",
        category: BotMcpAppCategory::Coding,
        composio_slugs: &[],
    },
    BotMcpAppCatalogEntry {
        id: "context7",
        name: "Context7",
        description: "Current docs and examples for code libraries",
        endpoint_url: "https://mcp.context7.com/mcp",
        auth_kind: BotMcpAuthKind::None,
        header_name: None,
        docs_url: "https://context7.com",
        category: BotMcpAppCategory::Coding,
        composio_slugs: &[],
    },
    BotMcpAppCatalogEntry {
        id: "atlassian",
        name: "Atlassian",
        description: "Jira issues and Confluence pages",
        endpoint_url: "https://mcp.atlassian.com/v2/mcp",
        auth_kind: BotMcpAuthKind::BrowserOAuth,
        header_name: None,
        docs_url: "https://support.atlassian.com/rovo/docs/getting-started-with-the-atlassian-remote-mcp-server/",
        category: BotMcpAppCategory::Docs,
        composio_slugs: &["jira", "confluence"],
    },
    BotMcpAppCatalogEntry {
        id: "notion",
        name: "Notion",
        description: "Search, read and edit pages",
        endpoint_url: "https://mcp.notion.com/mcp",
        auth_kind: BotMcpAuthKind::BrowserOAuth,
        header_name: None,
        docs_url: "https://developers.notion.com/docs/mcp",
        category: BotMcpAppCategory::Docs,
        composio_slugs: &["notion"],
    },
    BotMcpAppCatalogEntry {
        id: "stripe",
        name: "Stripe",
        description: "Look up payments, customers and subscriptions",
        endpoint_url: "https://mcp.stripe.com",
        auth_kind: BotMcpAuthKind::BrowserOAuth,
        header_name: None,
        docs_url: "https://docs.stripe.com/mcp",
        category: BotMcpAppCategory::Sales,
        composio_slugs: &["stripe"],
    },
];

/// All 24 curated Composio third-party apps adapted from Orca.
pub const COMPOSIO_CURATED_APPS: &[ComposioAppCatalogEntry] = &[
    ComposioAppCatalogEntry {
        slug: "outlook",
        name: "Outlook",
        description: "Mail, calendar and contacts",
    },
    ComposioAppCatalogEntry {
        slug: "microsoft_teams",
        name: "Teams",
        description: "Chats, channels and meetings",
    },
    ComposioAppCatalogEntry {
        slug: "notion",
        name: "Notion",
        description: "Pages, databases and comments",
    },
    ComposioAppCatalogEntry {
        slug: "jira",
        name: "Jira",
        description: "Issues, projects and sprints",
    },
    ComposioAppCatalogEntry {
        slug: "hubspot",
        name: "HubSpot",
        description: "Contacts, companies, deals and tickets",
    },
    ComposioAppCatalogEntry {
        slug: "salesforce",
        name: "Salesforce",
        description: "Accounts, leads and opportunities",
    },
    ComposioAppCatalogEntry {
        slug: "asana",
        name: "Asana",
        description: "Tasks, projects and comments",
    },
    ComposioAppCatalogEntry {
        slug: "airtable",
        name: "Airtable",
        description: "Bases, tables and records",
    },
    ComposioAppCatalogEntry {
        slug: "zendesk",
        name: "Zendesk",
        description: "Tickets, users and help center articles",
    },
    ComposioAppCatalogEntry {
        slug: "figma",
        name: "Figma",
        description: "Files, comments and components",
    },
    ComposioAppCatalogEntry {
        slug: "stripe",
        name: "Stripe",
        description: "Payments, customers and invoices",
    },
    ComposioAppCatalogEntry {
        slug: "dropbox",
        name: "Dropbox",
        description: "Files and folders",
    },
    ComposioAppCatalogEntry {
        slug: "one_drive",
        name: "OneDrive",
        description: "Files and folders",
    },
    ComposioAppCatalogEntry {
        slug: "confluence",
        name: "Confluence",
        description: "Pages, spaces and comments",
    },
    ComposioAppCatalogEntry {
        slug: "clickup",
        name: "ClickUp",
        description: "Tasks, lists and docs",
    },
    ComposioAppCatalogEntry {
        slug: "monday",
        name: "Monday",
        description: "Boards, items and updates",
    },
    ComposioAppCatalogEntry {
        slug: "intercom",
        name: "Intercom",
        description: "Conversations, contacts and articles",
    },
    ComposioAppCatalogEntry {
        slug: "zoom",
        name: "Zoom",
        description: "Meetings, recordings and transcripts",
    },
    ComposioAppCatalogEntry {
        slug: "calendly",
        name: "Calendly",
        description: "Events, invitees and booking links",
    },
    ComposioAppCatalogEntry {
        slug: "trello",
        name: "Trello",
        description: "Boards, lists and cards",
    },
    ComposioAppCatalogEntry {
        slug: "sentry",
        name: "Sentry",
        description: "Issues, events and releases",
    },
    ComposioAppCatalogEntry {
        slug: "box",
        name: "Box",
        description: "Files and folders",
    },
    ComposioAppCatalogEntry {
        slug: "miro",
        name: "Miro",
        description: "Boards, sticky notes and frames",
    },
    ComposioAppCatalogEntry {
        slug: "reddit",
        name: "Reddit",
        description: "Posts, comments and subreddits",
    },
];

/// Returns all 24 built-in bot templates.
#[must_use]
pub const fn builtin_bot_templates() -> &'static [BotTemplate] {
    BUILTIN_BOT_TEMPLATES
}

/// Finds a built-in bot template by ID.
#[must_use]
pub fn find_bot_template(id: &str) -> Option<&'static BotTemplate> {
    BUILTIN_BOT_TEMPLATES.iter().find(|t| t.id == id)
}

/// Returns all built-in templates belonging to `category`.
#[must_use]
pub fn bot_templates_by_category(category: BotTemplateCategory) -> Vec<&'static BotTemplate> {
    BUILTIN_BOT_TEMPLATES
        .iter()
        .filter(|t| t.category == category)
        .collect()
}

/// Returns all 13 bundled bot skills.
#[must_use]
pub const fn builtin_bot_skills() -> &'static [SkillEntry] {
    BUILTIN_BOT_SKILLS
}

/// Finds a bundled bot skill by name.
#[must_use]
pub fn find_bot_skill(name: &str) -> Option<&'static SkillEntry> {
    let trimmed = name.trim();
    BUILTIN_BOT_SKILLS.iter().find(|s| s.name == trimmed)
}

/// Finds a curated MCP app by ID.
#[must_use]
pub fn find_bot_mcp_app(id: &str) -> Option<&'static BotMcpAppCatalogEntry> {
    let trimmed = id.trim();
    BOT_MCP_APPS.iter().find(|app| app.id == trimmed)
}

/// Finds a curated Composio app by slug.
#[must_use]
pub fn find_composio_app(slug: &str) -> Option<&'static ComposioAppCatalogEntry> {
    let trimmed = slug.trim();
    COMPOSIO_CURATED_APPS.iter().find(|app| app.slug == trimmed)
}

/// Slack rejects a manifest whose `bot_user.display_name` has any non-ASCII character (Korean
/// included); spaces and capitals are fine. Keeps the ASCII words, else falls back to `Krill Bot`.
fn slack_bot_display_name(name: &str) -> String {
    let ascii: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                c
            } else {
                ' '
            }
        })
        .collect();
    let words = ascii.split_whitespace().collect::<Vec<_>>().join(" ");
    if words.chars().any(|c| c.is_ascii_alphanumeric()) {
        words
    } else {
        "Krill Bot".to_owned()
    }
}

/// Generates a ready-to-paste Slack App Manifest YAML configured for local Socket Mode
/// (no public webhook URL or hosted cloud relay required).
#[must_use]
pub fn generate_slack_socket_mode_manifest(bot_name: &str) -> String {
    let clean_name = sanitize_yaml_single_line(bot_name, 35, "Krill Bot");
    let escaped = escape_yaml_double_quoted(&clean_name);
    let display = escape_yaml_double_quoted(&slack_bot_display_name(&clean_name));
    [
        "display_information:".to_owned(),
        format!("  name: \"{escaped}\""),
        "  description: \"Local Krill Bot connected over Slack Socket Mode\"".to_owned(),
        "features:".to_owned(),
        // Without a writable Messages tab Slack makes the bot's DM read-only
        // (`restricted_action_read_only_channel`), so nobody can DM the bot.
        "  app_home:".to_owned(),
        "    home_tab_enabled: false".to_owned(),
        "    messages_tab_enabled: true".to_owned(),
        "    messages_tab_read_only_enabled: false".to_owned(),
        "  bot_user:".to_owned(),
        format!("    display_name: \"{display}\""),
        "    always_online: true".to_owned(),
        "oauth_config:".to_owned(),
        "  scopes:".to_owned(),
        "    bot:".to_owned(),
        "      - app_mentions:read".to_owned(),
        "      - channels:history".to_owned(),
        "      - channels:read".to_owned(),
        "      - chat:write".to_owned(),
        "      - files:read".to_owned(),
        "      - files:write".to_owned(),
        "      - groups:history".to_owned(),
        "      - groups:read".to_owned(),
        "      - im:history".to_owned(),
        "      - im:read".to_owned(),
        "      - im:write".to_owned(),
        "      - mpim:history".to_owned(),
        "      - mpim:read".to_owned(),
        "      - reactions:read".to_owned(),
        "      - reactions:write".to_owned(),
        "      - users:read".to_owned(),
        "settings:".to_owned(),
        "  event_subscriptions:".to_owned(),
        "    bot_events:".to_owned(),
        "      - app_mention".to_owned(),
        "      - message.channels".to_owned(),
        "      - message.groups".to_owned(),
        "      - message.im".to_owned(),
        "      - message.mpim".to_owned(),
        "  interactivity:".to_owned(),
        "    is_enabled: true".to_owned(),
        "  org_deploy_enabled: false".to_owned(),
        "  socket_mode_enabled: true".to_owned(),
        "  token_rotation_enabled: false".to_owned(),
        String::new(),
    ]
    .join("\n")
}

fn sanitize_yaml_single_line(raw: &str, max_chars: usize, fallback: &str) -> String {
    let cleaned: String = raw
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let truncated: String = cleaned.chars().take(max_chars).collect();
    let trimmed = truncated.trim();
    if trimmed.is_empty() {
        fallback.to_owned()
    } else {
        trimmed.to_owned()
    }
}

fn escape_yaml_double_quoted(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            other => out.push(other),
        }
    }
    out
}
