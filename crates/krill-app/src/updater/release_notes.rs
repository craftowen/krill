//! 설정 > 업데이트에 보이는 릴리스별 변경 사항.
//!
//! GitHub Releases 본문에서 사람이 읽을 항목만 추린다. 화면에 글자로만 쓰고 설치 판단에는 쓰지 않으므로,
//! 서명 대상인 `latest.json`을 넓히지 않는다. 메인테이너가 쓴 `Release highlights`가 있으면 그것을,
//! 없으면 `GitHub`가 만든 `What's Changed`의 PR 제목을, 그것도 없으면(PR 없이 올린 릴리스) `main`의 커밋 제목을 보여 준다.
//! 여기까지가 영어 목록이다. 한국어는 저장소의 `docs/releases/ko.json`에서 버전별로 받고, 없으면 영어를 보여 준다.

use std::path::Path;
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};

use super::{Error, fetch_bytes};
use crate::i18n::Language;
use crate::onboarding::state::atomic_write_0600;

const API_URL: &str = "https://api.github.com/repos/craftowen/krill/releases?per_page=20";
const COMMITS_URL: &str =
    "https://api.github.com/repos/craftowen/krill/commits?sha=main&per_page=100";
pub const RELEASES_PAGE_URL: &str = "https://github.com/craftowen/krill/releases";
const KOREAN_URL: &str =
    "https://raw.githubusercontent.com/craftowen/krill/main/docs/releases/ko.json";
const CACHE_FILE: &str = "release-notes.json";
const MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
/// 비로그인 GitHub API는 IP당 시간당 60회다. 갱신마다 최대 2회이므로 페이지를 자주 열어도 한도를 넘지 않는다.
const FRESH_FOR: Duration = Duration::from_mins(10);
/// 사용자에게 보이는 동작을 바꾸지 않는 PR 종류.
const HIDDEN_KINDS: [&str; 7] = ["build", "chore", "ci", "docs", "refactor", "style", "test"];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReleaseNote {
    pub version: String,
    /// `YYYY-MM-DD`(UTC).
    pub date: String,
    /// 영어 항목.
    pub items: Vec<String>,
    #[serde(default)]
    pub items_ko: Vec<String>,
}

impl ReleaseNote {
    /// 고른 언어의 항목. 번역이 없는 한국어는 영어로 대신한다.
    #[must_use]
    pub fn items_in(&self, language: Language) -> &[String] {
        if language == Language::Ko && !self.items_ko.is_empty() {
            &self.items_ko
        } else {
            &self.items
        }
    }
}

#[derive(Deserialize)]
struct GithubCommit {
    commit: GithubCommitBody,
}

#[derive(Deserialize)]
struct GithubCommitBody {
    message: String,
}

#[derive(Deserialize)]
struct GithubRelease {
    tag_name: String,
    #[serde(default)]
    published_at: Option<String>,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
}

/// GitHub `releases` 응답을 받은 순서(최신 먼저)대로 바꾼다. 초안과 사전 릴리스는 뺀다.
pub fn parse_releases(json: &[u8]) -> Result<Vec<ReleaseNote>, Error> {
    let releases: Vec<GithubRelease> =
        serde_json::from_slice(json).map_err(|e| Error::Json(e.to_string()))?;
    Ok(releases
        .into_iter()
        .filter(|r| !r.draft && !r.prerelease)
        .map(|r| ReleaseNote {
            version: r.tag_name.trim_start_matches('v').to_owned(),
            date: r
                .published_at
                .as_deref()
                .and_then(|at| at.get(..10))
                .unwrap_or_default()
                .to_owned(),
            items: parse_items(r.body.as_deref().unwrap_or_default()),
            items_ko: Vec::new(),
        })
        .collect())
}

/// 항목이 빈 릴리스를 커밋 제목으로 채운다. 최신 먼저인 목록에서 `Release vX` 뒤의 커밋이 X의 변경이다.
/// 응답 끝에서 잘린 묶음은 일부만 보여 주지 않도록 다음 `Release`를 만나야 확정한다.
fn fill_from_commits(notes: &mut [ReleaseNote], json: &[u8]) -> Result<(), Error> {
    let commits: Vec<GithubCommit> =
        serde_json::from_slice(json).map_err(|e| Error::Json(e.to_string()))?;
    let mut open: Option<(String, Vec<String>)> = None;
    for GithubCommit { commit } in commits {
        let title = commit.message.lines().next().unwrap_or_default();
        if let Some(version) = title.strip_prefix("Release v") {
            let next = (version.to_owned(), Vec::new());
            if let Some((version, items)) = open.replace(next)
                && let Some(note) = notes
                    .iter_mut()
                    .find(|note| note.version == version && note.items.is_empty())
            {
                note.items = items;
            }
        } else if let Some((_, items)) = &mut open {
            let title = title.rsplit_once(" (#").map_or(title, |(title, _)| title);
            items.extend(pr_title(title));
        }
    }
    Ok(())
}

/// `{"0.2.51": ["…"]}` 형식의 번역을 버전별로 붙인다.
fn apply_korean(notes: &mut [ReleaseNote], json: &[u8]) -> Result<(), Error> {
    let mut korean: std::collections::HashMap<String, Vec<String>> =
        serde_json::from_slice(json).map_err(|e| Error::Json(e.to_string()))?;
    for note in notes {
        if let Some(items) = korean.remove(&note.version) {
            note.items_ko = items;
        }
    }
    Ok(())
}

/// dev·main을 다시 합치면 같은 변경이 두 릴리스 범위에 잡힌다. 처음 나간(오래된) 릴리스에만 남긴다.
fn keep_first_shipped(notes: &mut [ReleaseNote]) {
    let mut seen = std::collections::HashSet::new();
    for note in notes.iter_mut().rev() {
        note.items.retain(|item| seen.insert(item.clone()));
    }
}

fn parse_items(body: &str) -> Vec<String> {
    let section = |title: &'static str| {
        body.lines()
            .skip_while(move |line| line.trim() != title)
            .skip(1)
            .take_while(|line| !line.starts_with("## "))
            .filter_map(bullet)
    };
    let highlights: Vec<String> = section("## Release highlights")
        .filter(|line| !has_hangul(line))
        .map(|line| line.replace("**", "").replace('`', ""))
        .collect();
    if highlights.is_empty() {
        section("## What's Changed").filter_map(pr_title).collect()
    } else {
        highlights
    }
}

/// 예전 릴리스에는 한글 하이라이트와 제목이 있다. 영어 목록에서 빼고 한국어는 `ko.json`이 맡는다.
fn has_hangul(text: &str) -> bool {
    text.chars().any(|c| ('가'..='힣').contains(&c))
}

fn bullet(line: &str) -> Option<&str> {
    let line = line.trim_start();
    line.strip_prefix("- ")
        .or_else(|| line.strip_prefix("* "))
        .map(str::trim)
        .filter(|text| !text.is_empty())
}

/// `fix(app): keep the cursor by @me in https://…` → `Keep the cursor`. 내부 작업 PR은 `None`.
fn pr_title(line: &str) -> Option<String> {
    let mut title = line.rsplit_once(" by @").map_or(line, |(title, _)| title);
    if let Some((prefix, rest)) = title.split_once(": ") {
        let kind = prefix
            .split('(')
            .next()
            .unwrap_or(prefix)
            .trim_end_matches('!');
        if kind.chars().all(|c| c.is_ascii_lowercase()) {
            if HIDDEN_KINDS.contains(&kind) {
                return None;
            }
            title = rest;
        }
    }
    if title.starts_with("Release v") || title.starts_with("Merge ") || has_hangul(title) {
        return None;
    }
    let mut chars = title.chars();
    let first = chars.next()?;
    let title: String = first.to_uppercase().chain(chars).collect();
    Some(title.replace('`', ""))
}

pub fn read_cache(state_dir: &Path) -> Option<Vec<ReleaseNote>> {
    serde_json::from_slice(&std::fs::read(state_dir.join(CACHE_FILE)).ok()?).ok()
}

#[must_use]
pub fn cache_is_fresh(state_dir: &Path) -> bool {
    std::fs::metadata(state_dir.join(CACHE_FILE))
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|at| SystemTime::now().duration_since(at).ok())
        .is_some_and(|age| age < FRESH_FOR)
}

/// `GitHub`에서 받아 캐시를 갱신한다. 캐시 쓰기 실패는 다음에 다시 받으면 되므로 오류로 치지 않는다.
pub fn fetch(state_dir: &Path) -> Result<Vec<ReleaseNote>, Error> {
    let mut notes = parse_releases(&fetch_bytes(API_URL, MAX_RESPONSE_BYTES)?)?;
    if notes.iter().any(|note| note.items.is_empty())
        && let Ok(commits) = fetch_bytes(COMMITS_URL, MAX_RESPONSE_BYTES)
    {
        // 못 채운 릴리스는 화면에서 GitHub 링크로 안내한다.
        fill_from_commits(&mut notes, &commits).ok();
    }
    keep_first_shipped(&mut notes);
    // 번역을 못 받아도 영어로 보여 주면 되므로 실패로 치지 않는다.
    if let Ok(korean) = fetch_bytes(KOREAN_URL, MAX_RESPONSE_BYTES) {
        apply_korean(&mut notes, &korean).ok();
    }
    if let Ok(body) = serde_json::to_string(&notes) {
        atomic_write_0600(&state_dir.join(CACHE_FILE), &body).ok();
    }
    Ok(notes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pr_titles_become_plain_items_and_internal_work_is_hidden() {
        let body = "# Krill v1\n\n## Release highlights\n\nNo release-specific highlights were supplied.\n\n## Changelog\n\nFoo\n\n\n\n## What's Changed\n\
* fix(krilld,app): keep the cursor blinking by @me in https://github.com/x/pull/1\r\n\
* feat!: add updates page by @me in https://github.com/x/pull/2\n\
* perf(app): draw less by @app/bot in https://github.com/x/pull/3\n\
* chore(packaging): remove a gate by @me in https://github.com/x/pull/4\n\
* test(krilld): isolate state by @me in https://github.com/x/pull/5\n\
* Release v0.2.51 by @me in https://github.com/x/pull/6\n\
* Fix: colon in a plain title by @me in https://github.com/x/pull/7\n\
* fix: 한글 제목 by @me in https://github.com/x/pull/8\n\
* Merge released main changes back into dev by @me in https://github.com/x/pull/9\n\
\n## New Contributors\n* @me made their first contribution\n";
        assert_eq!(
            parse_items(body),
            [
                "Keep the cursor blinking",
                "Add updates page",
                "Draw less",
                "Fix: colon in a plain title",
            ]
        );
    }

    #[test]
    fn empty_releases_are_filled_from_commits_between_release_commits() {
        let note = |version: &str, items: &[&str]| ReleaseNote {
            version: version.into(),
            date: String::new(),
            items: items.iter().map(|item| (*item).to_owned()).collect(),
            items_ko: Vec::new(),
        };
        let mut notes = [
            note("3.0.0", &["Written"]),
            note("2.0.0", &[]),
            note("1.0.0", &[]),
        ];
        let json = br#"[
            {"commit":{"message":"fix: unreleased"}},
            {"commit":{"message":"Release v3.0.0"}},
            {"commit":{"message":"feat: ignored because notes exist"}},
            {"commit":{"message":"Release v2.0.0\n\nbody"}},
            {"commit":{"message":"feat(theme): add themes (#12)\n\nbody"}},
            {"commit":{"message":"Merge branch 'dev'"}},
            {"commit":{"message":"chore: hidden"}},
            {"commit":{"message":"Release v1.0.0"}},
            {"commit":{"message":"fix: cut off by the page size"}}
        ]"#;
        fill_from_commits(&mut notes, json).unwrap();
        assert_eq!(notes[0].items, ["Written"]);
        assert_eq!(notes[1].items, ["Add themes"]);
        assert!(notes[2].items.is_empty());
    }

    #[test]
    fn a_change_listed_twice_stays_with_the_release_that_shipped_it_first() {
        let note = |version: &str, items: &[&str]| ReleaseNote {
            version: version.into(),
            date: String::new(),
            items: items.iter().map(|item| (*item).to_owned()).collect(),
            items_ko: Vec::new(),
        };
        let mut notes = [note("2.0.0", &["Same", "New"]), note("1.0.0", &["Same"])];
        keep_first_shipped(&mut notes);
        assert_eq!(notes[0].items, ["New"]);
        assert_eq!(notes[1].items, ["Same"]);
    }

    #[test]
    fn korean_items_follow_the_language_and_fall_back_to_english() {
        let mut notes = [
            ReleaseNote {
                version: "2.0.0".into(),
                date: String::new(),
                items: vec!["Fixed".into()],
                items_ko: vec![],
            },
            ReleaseNote {
                version: "1.0.0".into(),
                date: String::new(),
                items: vec!["Old".into()],
                items_ko: vec![],
            },
        ];
        apply_korean(&mut notes, r#"{"2.0.0":["고침"]}"#.as_bytes()).unwrap();
        assert_eq!(notes[0].items_in(Language::Ko), ["고침"]);
        assert_eq!(notes[0].items_in(Language::En), ["Fixed"]);
        assert_eq!(notes[1].items_in(Language::Ko), ["Old"]);
    }

    #[test]
    fn korean_highlights_stay_out_of_the_english_list() {
        let body = "## Release highlights\n\n- 세션을 유지합니다.\n\n## What's Changed\n* fix: keep sessions by @me in u\n";
        assert_eq!(parse_items(body), ["Keep sessions"]);
    }

    #[test]
    fn bundled_korean_notes_parse() {
        let json = include_str!("../../../../docs/releases/ko.json");
        let korean: std::collections::HashMap<String, Vec<String>> =
            serde_json::from_str(json).unwrap();
        assert!(korean.keys().all(|version| !version.starts_with('v')));
        assert!(korean.values().all(|items| !items.is_empty()));
    }

    #[test]
    fn written_highlights_win_over_pr_titles() {
        let body = "## Release highlights\n\n- **Sessions** stay `alive`.\n\n## Changelog\n\n## What's Changed\n* fix: a by @me in u\n";
        assert_eq!(parse_items(body), ["Sessions stay alive."]);
    }

    #[test]
    fn releases_skip_drafts_and_prereleases_and_tolerate_missing_fields() {
        let json = br#"[
            {"tag_name":"v0.3.0","draft":true,"body":"x"},
            {"tag_name":"v0.2.99","prerelease":true,"published_at":"2026-11-05T00:00:00Z"},
            {"tag_name":"v0.2.51","published_at":"2026-11-04T06:24:44Z","body":null,"assets":[]},
            {"tag_name":"v0.2.50","published_at":null}
        ]"#;
        assert_eq!(
            parse_releases(json).unwrap(),
            [
                ReleaseNote {
                    version: "0.2.51".into(),
                    date: "2026-11-04".into(),
                    items: vec![],
                    items_ko: vec![],
                },
                ReleaseNote {
                    version: "0.2.50".into(),
                    date: String::new(),
                    items: vec![],
                    items_ko: vec![],
                },
            ]
        );
        assert!(parse_releases(b"{\"message\":\"rate limit\"}").is_err());
    }
}
