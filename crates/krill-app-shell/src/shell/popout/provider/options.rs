//! 프로바이더별 사용량 상세(모델별 주간 창·한도 재평가 시각).

use super::*;

pub(super) fn advanced_notes(snap: &AccountsSnapshot, account: &AccountInfo) -> Vec<String> {
    use krill_app_core::i18n::catalog::shell as text;
    if !snap.advanced.supported {
        return Vec::new();
    }
    let now = krill_app_core::accounts::display::now();
    let mut notes = Vec::new();
    for model in snap
        .advanced
        .model_windows
        .iter()
        .filter(|m| m.account_id == account.id)
    {
        let pct = krill_app_core::accounts::display::window(
            Some(&model.seven_day),
            i64::try_from(now).unwrap_or(i64::MAX),
        )
        .text;
        notes.push(format!(
            "{} · {} {pct}",
            model.model,
            account_text::WINDOW_7D.get()
        ));
    }
    if let Some(limit) = snap
        .advanced
        .rate_limits
        .iter()
        .find(|l| l.account_id == account.id)
    {
        notes.push(
            text::ACCOUNT_LIMIT_CLEARS
                .format(&[("seconds", &limit.clears_at.saturating_sub(now).to_string())]),
        );
    }
    notes
}
