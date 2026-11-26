//! Reading a bot chat's prompts the way Orca draws them: which automation started a turn,
//! automation result/wait lines, created-bot receipts, and typed-by / connector-asker tags.

/// The bot schedule whose prompt started this turn (Orca's automation run row says
/// "Scheduled · {name}" over it).
pub(super) fn scheduled_run<'a>(
    prompt: &str,
    schedules: &'a [(String, String)],
) -> Option<&'a str> {
    let prompt = prompt.trim();
    schedules
        .iter()
        .find(|(_, scheduled)| !scheduled.trim().is_empty() && scheduled.trim() == prompt)
        .map(|(name, _)| name.as_str())
}

/// Optional sender tag above a user prompt (`NativeChatTypedByTag` / `NativeChatConnectorAskerTag`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum PromptSenderTag {
    TypedBy(String),
    ConnectorAsker { source: String, name: String },
}

/// Strips a leading `[Typed by <name>]` or `[via <source> · <name>]` tag from a user prompt.
pub(super) fn parse_prompt_sender_tag(text: &str) -> (Option<PromptSenderTag>, &str) {
    let trimmed = text.trim_start();
    if let Some(rest) = trimmed.strip_prefix("[Typed by ")
        && let Some((name, body)) = rest.split_once(']')
        && !name.trim().is_empty()
    {
        return (
            Some(PromptSenderTag::TypedBy(name.trim().to_owned())),
            body.trim_start_matches(['\n', '\r', ' ']),
        );
    }
    if let Some(rest) = trimmed.strip_prefix("[Connector:")
        && let Some((meta, body)) = rest.split_once(']')
        && let Some((source, name)) = meta.split_once('|')
        && !source.trim().is_empty()
        && !name.trim().is_empty()
    {
        return (
            Some(PromptSenderTag::ConnectorAsker {
                source: source.trim().to_owned(),
                name: name.trim().to_owned(),
            }),
            body.trim_start_matches(['\n', '\r', ' ']),
        );
    }
    (None, text)
}

/// Automation result or wait marker row (`NativeChatAutomationResultRow` / `NativeChatWaitRow`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum AutomationInlineRow {
    Result { name: String, summary: String },
    Wait { label: String },
}

pub(super) fn parse_automation_inline_row(text: &str) -> Option<AutomationInlineRow> {
    let trimmed = text.trim();
    if let Some(rest) = trimmed.strip_prefix("[Krill automation result: ")
        && let Some((name, summary)) = rest.split_once(']')
    {
        return Some(AutomationInlineRow::Result {
            name: name.trim().to_owned(),
            summary: summary.trim().to_owned(),
        });
    }
    if let Some(rest) = trimmed.strip_prefix("[Krill wait: ")
        && let Some(label) = rest.strip_suffix(']')
    {
        return Some(AutomationInlineRow::Wait {
            label: label.trim().to_owned(),
        });
    }
    None
}

/// Bot created event row (`NativeChatBotCreatedRow`).
pub(super) fn parse_bot_created_row(text: &str) -> Option<(String, Option<String>)> {
    let trimmed = text.trim();
    let rest = trimmed.strip_prefix("[Krill bot created: ")?;
    let inner = rest.strip_suffix(']')?;
    if let Some((name, id)) = inner.split_once('|') {
        Some((name.trim().to_owned(), Some(id.trim().to_owned())))
    } else if !inner.trim().is_empty() {
        Some((inner.trim().to_owned(), None))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_turn_a_schedule_started_names_it() {
        let schedules = vec![
            ("Morning triage".to_owned(), "Triage new issues.".to_owned()),
            ("Empty".to_owned(), String::new()),
        ];
        assert_eq!(
            scheduled_run("Triage new issues.\n", &schedules),
            Some("Morning triage")
        );
        assert_eq!(scheduled_run("", &schedules), None);
        assert_eq!(scheduled_run("Something else", &schedules), None);
    }

    #[test]
    fn parses_sender_tags_and_automation_rows() {
        assert_eq!(
            parse_prompt_sender_tag("[Typed by Alice]\nRun tests"),
            (Some(PromptSenderTag::TypedBy("Alice".into())), "Run tests")
        );
        assert_eq!(
            parse_prompt_sender_tag("[Connector:Slack|Bob] Check PR"),
            (
                Some(PromptSenderTag::ConnectorAsker {
                    source: "Slack".into(),
                    name: "Bob".into()
                }),
                "Check PR"
            )
        );
        assert_eq!(
            parse_automation_inline_row("[Krill automation result: Daily] 3 PRs merged"),
            Some(AutomationInlineRow::Result {
                name: "Daily".into(),
                summary: "3 PRs merged".into()
            })
        );
        assert_eq!(
            parse_automation_inline_row("[Krill wait: Waiting for CI]"),
            Some(AutomationInlineRow::Wait {
                label: "Waiting for CI".into()
            })
        );
        assert_eq!(
            parse_bot_created_row("[Krill bot created: ReviewBot|bot-1]"),
            Some(("ReviewBot".into(), Some("bot-1".into())))
        );
    }
}
