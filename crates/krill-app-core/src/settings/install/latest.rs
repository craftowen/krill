//! 설치된 도구의 업스트림 최신 버전 조회 및 semver 비교.
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde_json::Value;

use super::ToolStatus;

const CACHE_TTL: Duration = Duration::from_mins(10);
const CURL_TIMEOUT: Duration = Duration::from_secs(5);

static LATEST_CACHE: OnceLock<Mutex<HashMap<String, (Instant, String)>>> = OnceLock::new();

/// 설치된 도구와 업스트림 최신 버전 비교 상태.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateAvailability {
    NotInstalled,
    UpToDate,
    UpdateAvailable { latest: String },
    Unknown,
}

impl ToolStatus {
    /// 설치된 버전과 업스트림 최신 버전을 비교해 업데이트 가능 여부를 돌려준다.
    #[must_use]
    pub fn update_availability(&self) -> UpdateAvailability {
        if !self.installed {
            return UpdateAvailability::NotInstalled;
        }
        let Some(current) = self.version.as_deref().and_then(extract_semver) else {
            return UpdateAvailability::Unknown;
        };
        let Some(latest) = self.latest_version.as_deref().and_then(extract_semver) else {
            return UpdateAvailability::Unknown;
        };
        match krill_proto::control::is_newer(&latest, &current) {
            Some(true) => UpdateAvailability::UpdateAvailable { latest },
            Some(false) => UpdateAvailability::UpToDate,
            None => UpdateAvailability::Unknown,
        }
    }
}

/// `--version` 출력이나 릴리스 태그 문자열에서 비교 가능한 semver(`X.Y.Z[-pre]`)를 추출한다.
#[must_use]
pub fn extract_semver(raw: &str) -> Option<String> {
    raw.split_whitespace().find_map(normalize_version_token)
}

fn normalize_version_token(raw: &str) -> Option<String> {
    let trimmed = raw.trim_matches(|c: char| matches!(c, '(' | ')' | ',' | ';' | '\u{27}' | '"'));
    let stripped_v = trimmed
        .strip_prefix('v')
        .or_else(|| trimmed.strip_prefix('V'))
        .filter(|rest| rest.as_bytes().first().is_some_and(u8::is_ascii_digit))
        .unwrap_or(trimmed);
    let without_build = stripped_v
        .split_once('+')
        .map_or(stripped_v, |(head, _)| head);
    let without_windows = without_build
        .split_once(".windows.")
        .map_or(without_build, |(head, _)| head);
    let without_revision = strip_brew_revision(without_windows);
    let candidate = strip_git_describe_suffix(without_revision);
    (krill_proto::control::is_newer(candidate, candidate) == Some(false))
        .then(|| candidate.to_owned())
}

fn strip_brew_revision(s: &str) -> &str {
    if let Some((head, rev)) = s.rsplit_once('_')
        && !rev.is_empty()
        && rev.bytes().all(|b| b.is_ascii_digit())
    {
        head
    } else {
        s
    }
}

fn strip_git_describe_suffix(s: &str) -> &str {
    let base = s.strip_suffix("-dirty").unwrap_or(s);
    if let Some((head, rest)) = base.split_once('-')
        && let Some((count, hash)) = rest.split_once("-g")
        && !count.is_empty()
        && count.bytes().all(|b| b.is_ascii_digit())
        && hash.len() >= 4
        && hash.bytes().all(|b| b.is_ascii_hexdigit())
    {
        head
    } else {
        s
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) enum ProbeSource {
    PlainText { url: String },
    JsonField { url: String, pointer: &'static str },
    NodeLts { url: String },
    GithubLatestRedirect { url: String },
}

impl ProbeSource {
    fn cache_key(&self) -> String {
        match self {
            Self::PlainText { url } => format!("text:{url}"),
            Self::JsonField { url, pointer } => format!("json:{url}:{pointer}"),
            Self::NodeLts { url } => format!("node-lts:{url}"),
            Self::GithubLatestRedirect { url } => format!("gh:{url}"),
        }
    }
}

pub(super) fn source_for_tool(tool_id: &str, status: &ToolStatus) -> Option<ProbeSource> {
    if !status.installed {
        return None;
    }
    let command = status.command.as_deref().unwrap_or("");
    source_for(tool_id, command, cfg!(windows))
}

fn source_for(tool_id: &str, command: &str, is_windows: bool) -> Option<ProbeSource> {
    if !is_windows {
        if let Some(cask) = brew_cask_name(command) {
            return Some(ProbeSource::JsonField {
                url: format!("https://formulae.brew.sh/api/cask/{cask}.json"),
                pointer: "/version",
            });
        }
        if let Some(formula) = brew_formula_name(command) {
            return Some(ProbeSource::JsonField {
                url: format!("https://formulae.brew.sh/api/formula/{formula}.json"),
                pointer: "/versions/stable",
            });
        }
    }
    match (tool_id, is_windows) {
        ("claude", _) => Some(ProbeSource::PlainText {
            url: "https://downloads.claude.ai/claude-code-releases/latest".to_owned(),
        }),
        ("codex", _) => Some(ProbeSource::JsonField {
            url: "https://registry.npmjs.org/@openai/codex/latest".to_owned(),
            pointer: "/version",
        }),
        ("agy", _) => Some(ProbeSource::JsonField {
            url: format!(
                "https://antigravity-cli-auto-updater-974169037036.us-central1.run.app/manifests/{}.json",
                agy_platform()
            ),
            pointer: "/version",
        }),
        ("git", false) => Some(ProbeSource::JsonField {
            url: "https://formulae.brew.sh/api/formula/git.json".to_owned(),
            pointer: "/versions/stable",
        }),
        ("git", true) => Some(ProbeSource::GithubLatestRedirect {
            url: "https://github.com/git-for-windows/git/releases/latest".to_owned(),
        }),
        ("node", false) => Some(ProbeSource::JsonField {
            url: "https://formulae.brew.sh/api/formula/node.json".to_owned(),
            pointer: "/versions/stable",
        }),
        ("node", true) => Some(ProbeSource::NodeLts {
            url: "https://nodejs.org/dist/index.json".to_owned(),
        }),
        ("gh", false) => Some(ProbeSource::JsonField {
            url: "https://formulae.brew.sh/api/formula/gh.json".to_owned(),
            pointer: "/versions/stable",
        }),
        ("gh", true) => Some(ProbeSource::GithubLatestRedirect {
            url: "https://github.com/cli/cli/releases/latest".to_owned(),
        }),
        ("pkgmgr", false) => Some(ProbeSource::GithubLatestRedirect {
            url: "https://github.com/Homebrew/brew/releases/latest".to_owned(),
        }),
        ("pkgmgr", true) => Some(ProbeSource::GithubLatestRedirect {
            url: "https://github.com/microsoft/winget-cli/releases/latest".to_owned(),
        }),
        ("uv", _) => Some(ProbeSource::GithubLatestRedirect {
            url: "https://github.com/astral-sh/uv/releases/latest".to_owned(),
        }),
        ("bun", _) => Some(ProbeSource::JsonField {
            url: "https://registry.npmjs.org/bun/latest".to_owned(),
            pointer: "/version",
        }),
        ("pnpm", _) => Some(ProbeSource::JsonField {
            url: "https://registry.npmjs.org/pnpm/latest".to_owned(),
            pointer: "/version",
        }),
        _ => None,
    }
}

fn brew_cask_name(command: &str) -> Option<&str> {
    if command.contains("claude-code@latest") || command.contains("'claude-code'") {
        return Some("claude-code@latest");
    }
    extract_brew_arg(command, "--cask ")
}

fn brew_formula_name(command: &str) -> Option<&str> {
    extract_brew_arg(command, "--formula ")
}

fn extract_brew_arg<'a>(command: &'a str, flag: &str) -> Option<&'a str> {
    let rest = command.split(flag).nth(1)?;
    let token = rest
        .split_whitespace()
        .next()?
        .trim_matches(|c| matches!(c, '\u{27}' | '"'));
    (!token.is_empty()).then_some(token)
}

const fn agy_platform() -> &'static str {
    match (
        cfg!(target_os = "macos"),
        cfg!(windows),
        cfg!(target_arch = "aarch64"),
    ) {
        (true, _, true) => "darwin_arm64",
        (true, _, false) => "darwin_amd64",
        (_, true, true) => "windows_arm64",
        (_, true, false) => "windows_amd64",
        (false, false, true) => "linux_arm64",
        (false, false, false) => "linux_amd64",
    }
}

/// 설치된 도구들의 업스트림 최신 버전을 병렬 조회해 `statuses`에 채운다.
pub(super) fn populate_latest_versions(
    statuses: &mut std::collections::BTreeMap<&'static str, ToolStatus>,
    force_refresh: bool,
) {
    if cfg!(test) {
        return;
    }

    let targets: Vec<(&'static str, ProbeSource)> = statuses
        .iter()
        .filter_map(|(&id, status)| source_for_tool(id, status).map(|src| (id, src)))
        .collect();
    if targets.is_empty() {
        return;
    }

    let now = Instant::now();
    let mut resolved: HashMap<String, String> = HashMap::new();
    let mut to_fetch: HashMap<String, ProbeSource> = HashMap::new();

    if force_refresh {
        for (_, src) in &targets {
            to_fetch
                .entry(src.cache_key())
                .or_insert_with(|| src.clone());
        }
    } else {
        let cache = LATEST_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
        let guard = cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for (_, src) in &targets {
            let key = src.cache_key();
            if let Some((fetched_at, version)) = guard.get(&key)
                && now.duration_since(*fetched_at) < CACHE_TTL
            {
                resolved.insert(key, version.clone());
                continue;
            }
            to_fetch.entry(key).or_insert_with(|| src.clone());
        }
    }

    if !to_fetch.is_empty() {
        let fresh: Vec<(String, String)> = std::thread::scope(|scope| {
            let handles: Vec<_> = to_fetch
                .into_iter()
                .map(|(key, src)| scope.spawn(move || fetch_source(&src).map(|ver| (key, ver))))
                .collect();
            handles
                .into_iter()
                .filter_map(|h| h.join().ok().flatten())
                .collect()
        });

        let fetched_at = Instant::now();
        let cache = LATEST_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
        let mut guard = cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for (key, ver) in fresh {
            guard.insert(key.clone(), (fetched_at, ver.clone()));
            resolved.insert(key, ver);
        }
    }

    for (id, src) in targets {
        if let Some(version) = resolved.get(&src.cache_key())
            && let Some(status) = statuses.get_mut(id)
        {
            status.latest_version = Some(version.clone());
        }
    }
}

fn fetch_source(source: &ProbeSource) -> Option<String> {
    let curl = if cfg!(windows) {
        "curl.exe"
    } else {
        "/usr/bin/curl"
    };
    let null_sink = if cfg!(windows) { "NUL" } else { "/dev/null" };
    match source {
        ProbeSource::GithubLatestRedirect { url } => {
            let mut cmd = crate::platform::background_command(curl);
            cmd.args([
                "-sS",
                "--max-time",
                "4",
                "-o",
                null_sink,
                "-w",
                "%{redirect_url}",
                url,
            ]);
            let out = super::super::child::output(&mut cmd, CURL_TIMEOUT, 2048).ok()?;
            if !out.status.success() {
                return None;
            }
            let redirect = String::from_utf8_lossy(&out.stdout);
            let tag = redirect.trim().rsplit("/releases/tag/").next()?;
            let decoded = tag.replace("%2B", "+").replace("%2b", "+");
            extract_semver(&decoded)
        }
        ProbeSource::PlainText { url } => {
            let bytes = fetch_body(curl, url, 64 * 1024)?;
            let text = String::from_utf8_lossy(&bytes);
            extract_semver(&text)
        }
        ProbeSource::JsonField { url, pointer } => {
            let bytes = fetch_body(curl, url, 128 * 1024)?;
            let value: Value = serde_json::from_slice(&bytes).ok()?;
            let raw = value.pointer(pointer).and_then(Value::as_str)?;
            extract_semver(raw)
        }
        ProbeSource::NodeLts { url } => {
            let bytes = fetch_body(curl, url, 512 * 1024)?;
            parse_node_lts_json(&bytes)
        }
    }
}

fn fetch_body(curl: &str, url: &str, max_bytes: u64) -> Option<Vec<u8>> {
    let mut cmd = crate::platform::background_command(curl);
    cmd.args(["-fsSL", "--max-time", "4", url]);
    let out = super::super::child::output(&mut cmd, CURL_TIMEOUT, max_bytes).ok()?;
    out.status.success().then_some(out.stdout)
}

fn parse_node_lts_json(bytes: &[u8]) -> Option<String> {
    let list = serde_json::from_slice::<Value>(bytes).ok()?;
    let arr = list.as_array()?;
    let lts_entry = arr.iter().find(|entry| {
        entry.get("lts").is_some_and(|lts| {
            lts.as_str().is_some_and(|s| !s.is_empty()) || lts.as_bool() == Some(true)
        })
    })?;
    let raw = lts_entry.get("version").and_then(Value::as_str)?;
    extract_semver(raw)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_semver_parses_all_tool_and_upstream_formats() {
        let cases = [
            ("2.1.323 (Claude Code)", Some("2.1.323")),
            ("codex-cli 0.167.2", Some("0.167.2")),
            ("1.5.8", Some("1.5.8")),
            ("git version 2.56.1", Some("2.56.1")),
            ("git version 2.56.1.windows.1", Some("2.56.1")),
            ("v2.56.1.windows.1", Some("2.56.1")),
            ("git version 2.39.5 (Apple Git-154)", Some("2.39.5")),
            ("v26.13.0", Some("26.13.0")),
            ("gh version 2.103.3 (2026-10-26)", Some("2.103.3")),
            ("Homebrew 7.0.13", Some("7.0.13")),
            ("Homebrew 7.0.13-15-gabcdef0", Some("7.0.13")),
            ("v1.29.30", Some("1.29.30")),
            ("uv 0.13.10 (67fa92fd5 2026-11-04)", Some("0.13.10")),
            ("1.5.12", Some("1.5.12")),
            ("10.15.0", Some("10.15.0")),
            ("2.56.0_2", Some("2.56.0")),
            ("1.0.0-alpha.1", Some("1.0.0-alpha.1")),
            ("설치됨", None),
            ("codex-test", None),
        ];
        for (input, expected) in cases {
            assert_eq!(extract_semver(input).as_deref(), expected, "{input}");
        }
    }

    #[test]
    fn update_availability_distinguishes_up_to_date_outdated_and_unknown() {
        let mut status = ToolStatus::default();
        assert_eq!(
            status.update_availability(),
            UpdateAvailability::NotInstalled
        );

        status.installed = true;
        status.version = Some("codex-cli 0.167.2".to_owned());
        assert_eq!(status.update_availability(), UpdateAvailability::Unknown);

        status.latest_version = Some("0.167.2".to_owned());
        assert_eq!(status.update_availability(), UpdateAvailability::UpToDate);

        status.latest_version = Some("0.168.0".to_owned());
        assert_eq!(
            status.update_availability(),
            UpdateAvailability::UpdateAvailable {
                latest: "0.168.0".to_owned()
            }
        );

        status.version = Some("0.169.0".to_owned());
        status.latest_version = Some("0.168.0".to_owned());
        assert_eq!(status.update_availability(), UpdateAvailability::UpToDate);

        status.version = Some("codex-test".to_owned());
        assert_eq!(status.update_availability(), UpdateAvailability::Unknown);
    }

    #[test]
    fn source_selection_follows_install_channel() {
        assert_eq!(
            source_for(
                "claude",
                "'/opt/homebrew/bin/brew' update && '/opt/homebrew/bin/brew' fetch --cask 'claude-code@latest'",
                false
            ),
            Some(ProbeSource::JsonField {
                url: "https://formulae.brew.sh/api/cask/claude-code@latest.json".to_owned(),
                pointer: "/version",
            })
        );
        assert_eq!(
            source_for(
                "claude",
                "curl -fsSL https://claude.ai/install.sh | bash -s -- latest",
                false
            ),
            Some(ProbeSource::PlainText {
                url: "https://downloads.claude.ai/claude-code-releases/latest".to_owned(),
            })
        );
        assert_eq!(
            source_for(
                "codex",
                "'/opt/homebrew/bin/brew' update && '/opt/homebrew/bin/brew' upgrade --cask 'codex'",
                false
            ),
            Some(ProbeSource::JsonField {
                url: "https://formulae.brew.sh/api/cask/codex.json".to_owned(),
                pointer: "/version",
            })
        );
        assert_eq!(
            source_for("codex", "npm install -g @openai/codex@latest", false),
            Some(ProbeSource::JsonField {
                url: "https://registry.npmjs.org/@openai/codex/latest".to_owned(),
                pointer: "/version",
            })
        );
        assert_eq!(
            source_for(
                "node",
                "'/opt/homebrew/bin/brew' upgrade --formula 'node@22'",
                false
            ),
            Some(ProbeSource::JsonField {
                url: "https://formulae.brew.sh/api/formula/node@22.json".to_owned(),
                pointer: "/versions/stable",
            })
        );
    }

    #[test]
    fn node_lts_parser_picks_first_active_lts_entry() {
        let json = br#"[
            {"version": "v25.1.0", "lts": false},
            {"version": "v24.11.1", "lts": "Jod"},
            {"version": "v22.19.0", "lts": "Iron"}
        ]"#;
        assert_eq!(parse_node_lts_json(json).as_deref(), Some("24.11.1"));
    }
}
