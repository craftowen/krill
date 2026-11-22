//! Built-in model lists the chat composer and the New bot dialog show until krilld reports the
//! agent's own catalog (Codex `models_cache.json`, `agy models`). One list, so the two never drift.

/// One built-in model. `id` is what the agent CLI's `--model` accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuiltinModel {
    pub id: &'static str,
    pub label: &'static str,
    pub efforts: &'static [&'static str],
    pub default_effort: Option<&'static str>,
}

const LMH: &[&str] = &["low", "medium", "high"];
const LMHX: &[&str] = &["low", "medium", "high", "xhigh"];
const LMHXM: &[&str] = &["low", "medium", "high", "xhigh", "max"];

const fn model(
    id: &'static str,
    label: &'static str,
    efforts: &'static [&'static str],
) -> BuiltinModel {
    BuiltinModel {
        id,
        label,
        efforts,
        default_effort: None,
    }
}

const CLAUDE: &[BuiltinModel] = &[
    model("default", "Default", LMHXM),
    model("opus", "Opus 5.5", LMHXM),
    model("fable", "Fable 5.2", LMHXM),
    model("sonnet", "Sonnet 5.5", LMHXM),
    model("haiku", "Haiku 5.5", LMH),
];

const CODEX: &[BuiltinModel] = &[
    BuiltinModel {
        default_effort: Some("high"),
        ..model("gpt-5.4", "GPT-5.4", LMHX)
    },
    BuiltinModel {
        default_effort: Some("high"),
        ..model("gpt-5.3-codex", "GPT-5.3 Codex", LMHX)
    },
    BuiltinModel {
        default_effort: Some("medium"),
        ..model("gpt-5.4-mini", "GPT-5.4 Mini", LMH)
    },
];

/// Built-in models and efforts for `agent`. Antigravity has no built-in models: its ids change
/// with every account and CLI release, so only `agy models` (via krilld) can name them.
#[must_use]
pub fn builtin(agent: &str) -> (&'static [BuiltinModel], &'static [&'static str]) {
    match agent.to_ascii_lowercase().as_str() {
        "codex" => (CODEX, LMHX),
        "gemini" | "agy" | "antigravity" => (&[], LMHXM),
        _ => (CLAUDE, LMHXM),
    }
}

/// Whether krilld's discovered list replaces the built-in one. Claude Code reports only the
/// models a user configured, so its native aliases stay listed (the composer's rule too).
#[must_use]
pub fn prefers_discovered(agent: &str) -> bool {
    !agent.eq_ignore_ascii_case("claude")
}
