//! Reading a bot chat's prompts the way Orca draws them: which automation started a turn.

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
}
