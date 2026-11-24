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
    ["LC_ALL", "LC_MESSAGES", "LANG"]
        .into_iter()
        .filter_map(|name| std::env::var(name).ok())
        .find(|locale| !locale.trim().is_empty())
}
