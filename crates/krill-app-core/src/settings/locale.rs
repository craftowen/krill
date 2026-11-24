//! First-launch language selection. Persisted settings never pass through this resolver.

use super::Language;

/// Resolve once per process: changing the OS language takes effect on the next app launch.
pub(super) fn system_language() -> Language {
    static LANGUAGE: std::sync::OnceLock<Language> = std::sync::OnceLock::new();
    *LANGUAGE.get_or_init(|| language_from_locale(preferred_locale().as_deref()))
}

/// Match the primary language subtag, accepting BCP 47 and POSIX locale spellings.
pub(super) fn language_from_locale(locale: Option<&str>) -> Language {
    let primary = locale
        .unwrap_or_default()
        .trim()
        .split(['-', '_', '.', '@'])
        .next()
        .unwrap_or_default();
    if primary.eq_ignore_ascii_case("ko") {
        Language::Ko
    } else {
        Language::En
    }
}

#[cfg(target_os = "macos")]
fn preferred_locale() -> Option<String> {
    // Finder launches need the native preference; shell LANG may be absent or unrelated.
    // Only the first preference determines the default, even if Korean appears later.
    objc2_foundation::NSLocale::preferredLanguages()
        .firstObject()
        .map(|language| language.to_string())
}

#[cfg(windows)]
fn preferred_locale() -> Option<String> {
    crate::platform::preferred_locale()
}

#[cfg(not(any(target_os = "macos", windows)))]
fn preferred_locale() -> Option<String> {
    preferred_locale_from_env(|name| std::env::var(name).ok())
}

/// GNU gettext precedence on Linux/Unix: `LANGUAGE` (colon-separated preference list) wins
/// unless the effective POSIX locale (`LC_ALL`, `LC_MESSAGES`, or `LANG`) is `C`/`POSIX`.
#[cfg(any(test, not(any(target_os = "macos", windows))))]
pub(super) fn preferred_locale_from_env(
    mut get: impl FnMut(&str) -> Option<String>,
) -> Option<String> {
    let lc_all = get("LC_ALL").filter(|s| !s.trim().is_empty());
    let lc_messages = get("LC_MESSAGES").filter(|s| !s.trim().is_empty());
    let lang = get("LANG").filter(|s| !s.trim().is_empty());
    let effective_posix = lc_all
        .as_deref()
        .or(lc_messages.as_deref())
        .or(lang.as_deref());
    let is_c_posix = effective_posix.is_some_and(|value| {
        let trimmed = value.trim();
        trimmed.eq_ignore_ascii_case("C")
            || trimmed.eq_ignore_ascii_case("POSIX")
            || trimmed.starts_with("C.")
            || trimmed.starts_with("c.")
    });
    if !is_c_posix
        && let Some(language) = get("LANGUAGE")
        && let Some(first) = language
            .split(':')
            .map(str::trim)
            .find(|entry| !entry.is_empty())
    {
        return Some(first.to_owned());
    }
    lc_all.or(lc_messages).or(lang)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linux_locale_env_respects_language_list_and_posix_c_override() {
        let lookup = |pairs: &[(&str, &str)]| {
            preferred_locale_from_env(|name| {
                pairs
                    .iter()
                    .find(|(k, _)| *k == name)
                    .map(|(_, v)| (*v).to_owned())
            })
        };
        // LANGUAGE colon-separated preference list wins on Linux when POSIX locale isn't C/POSIX.
        assert_eq!(
            lookup(&[("LANGUAGE", "ko:en"), ("LANG", "en_US.UTF-8")]).as_deref(),
            Some("ko")
        );
        assert_eq!(
            lookup(&[("LANGUAGE", ":ko_KR:en"), ("LANG", "en_US.UTF-8")]).as_deref(),
            Some("ko_KR")
        );
        // GNU gettext rule: C / POSIX / C.* disables LANGUAGE.
        assert_eq!(
            lookup(&[("LANGUAGE", "ko:en"), ("LC_ALL", "C")]).as_deref(),
            Some("C")
        );
        assert_eq!(
            lookup(&[("LANGUAGE", "ko:en"), ("LC_MESSAGES", "POSIX")]).as_deref(),
            Some("POSIX")
        );
        assert_eq!(
            lookup(&[("LANGUAGE", "ko:en"), ("LANG", "C.UTF-8")]).as_deref(),
            Some("C.UTF-8")
        );
        // Fallback order: LC_ALL > LC_MESSAGES > LANG when LANGUAGE is unset or empty.
        assert_eq!(
            lookup(&[
                ("LANGUAGE", ""),
                ("LC_ALL", "ko_KR.UTF-8"),
                ("LANG", "en_US.UTF-8"),
            ])
            .as_deref(),
            Some("ko_KR.UTF-8")
        );
        assert_eq!(
            lookup(&[("LC_MESSAGES", "ko_KR.UTF-8"), ("LANG", "en_US.UTF-8")]).as_deref(),
            Some("ko_KR.UTF-8")
        );
        assert_eq!(
            lookup(&[("LANG", "en_US.UTF-8")]).as_deref(),
            Some("en_US.UTF-8")
        );
        assert_eq!(lookup(&[]), None);
    }
}
