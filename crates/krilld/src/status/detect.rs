//! MK-049의 순수 보조 감지 규칙. 화면은 원본 바이트가 아니라 엔진이 만든 현재 가시 화면이다.
//! 제목 글리프는 MK-006 Claude 2.1.299·Codex 녹화본에 근거한다. 모르는 제목은 추측하지 않는다.
//!
//! 문구·글리프 지식은 [`ScreenRules`]에 있고 서명 규칙 파일(MK-091)로 교체할 수 있다.
//! 기본값은 내장 규칙이다.

use std::time::{Duration, Instant};

use krill_proto::agent::rules::{MenuHit, ScreenRules};
use krill_proto::control::{AgentState, AgentStatus, Confidence, Source};

/// 화면 규칙은 출력이 잠깐 멎은 뒤에만 적용한다(부분 재그리기를 대화상자 소실로 오인하지 않는다).
pub const QUIET: Duration = Duration::from_millis(300);

/// 대화상자를 한 번도 못 본 Claude hook waiting을 떠도는 hook으로 보는 유예. 실제 대화상자는 이 안에 그려진다.
pub const STRAY_WAITING: Duration = Duration::from_secs(5);

/// 제목·화면으로 얻은 상태. 프로세스 종료와 hook은 별도 경로다.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Evidence {
    pub state: AgentState,
    pub source: Source,
    pub confidence: Confidence,
    /// 같은 hook waiting 동안 실제 대화상자를 봤고, 그 뒤 프롬프트 복귀를 확인했다.
    pub(crate) dismissed_since: Option<u64>,
}

/// UI가 출처·신뢰도와 함께 표시할 배지. hook·확정 종료에는 붙이지 않는다.
pub fn badge(status: &AgentStatus) -> Option<&'static str> {
    status.badge()
}

/// 세션별 화면 이력. 저장하지 않는다: 재기동 직후에는 waiting 해제를 위해 대화상자를 다시 봐야 한다.
#[derive(Debug)]
pub struct Detector {
    revision: Option<u64>,
    changed: Option<Instant>,
    /// Codex의 일반 제목은 idle 때 글리프가 없다. 스피너를 실제 본 세션에서만 Done으로 해석한다.
    spinning_title: Option<String>,
    dialog_waiting_since: Option<u64>,
    /// 대화상자 없이 관찰 중인 hook waiting(`since_unix_ns`)과 처음 본 시각.
    stray_waiting: Option<(u64, Instant)>,
    rules: ScreenRules,
}

impl Default for Detector {
    fn default() -> Self {
        Self::with_rules(ScreenRules::default())
    }
}

impl Detector {
    /// 서명 규칙(MK-091)으로 화면 지식을 교체한다. 앱 업데이트 없이 바뀐다.
    pub fn with_rules(rules: ScreenRules) -> Self {
        Self {
            revision: None,
            changed: None,
            spinning_title: None,
            dialog_waiting_since: None,
            stray_waiting: None,
            rules,
        }
    }

    /// 현재 쓰는 화면 규칙.
    pub fn rules(&self) -> &ScreenRules {
        &self.rules
    }

    /// 화면 규칙을 바꾼다. 서명 파일이 교체되면 감시 루프가 호출한다.
    pub fn set_rules(&mut self, rules: ScreenRules) {
        self.rules = rules;
    }
}

impl Detector {
    pub fn observe(
        &mut self,
        title: &str,
        screen: &str,
        revision: u64,
        status: &AgentStatus,
        now: Instant,
    ) -> Option<Evidence> {
        if self.revision != Some(revision) {
            self.revision = Some(revision);
            self.changed = Some(now);
        }
        let quiet = self
            .changed
            .is_some_and(|at| now.saturating_duration_since(at) >= QUIET);
        let menu = self.rules.menu_hit(screen).map(|hit| match hit {
            MenuHit::Blocked => AgentState::Blocked,
            MenuHit::Waiting => AgentState::Waiting,
        });
        let title_state = self.title_state(title);
        if matches!(status.state, AgentState::Waiting | AgentState::Blocked)
            && matches!(status.source, Some(Source::Hook | Source::Screen))
        {
            if menu.is_some() {
                self.dialog_waiting_since = Some(status.since_unix_ns);
            }
            // 턴이 끝난 뒤 들어온 보조 프로세스 hook 등으로 대화상자 없는 waiting이 남으면 다음 hook까지
            // '확인 필요'에 갇힌다. Claude는 대화상자가 입력줄을 대신하므로 빈 입력줄이 계속 보이면 푼다.
            // ponytail: Claude만. 다른 에이전트는 질문 UI가 메뉴 규칙에 다 잡히는지 확인한 뒤 넓힌다.
            if status.source == Some(Source::Hook)
                && status.agent == "claude"
                && self.dialog_waiting_since != Some(status.since_unix_ns)
            {
                let first = match self.stray_waiting {
                    Some((since, at)) if since == status.since_unix_ns => at,
                    _ => {
                        self.stray_waiting = Some((status.since_unix_ns, now));
                        now
                    }
                };
                if quiet
                    && menu.is_none()
                    && self.rules.is_prompt(screen)
                    && now.saturating_duration_since(first) >= STRAY_WAITING
                {
                    self.stray_waiting = None;
                    let state = if title_state == Some(AgentState::Working) {
                        AgentState::Working
                    } else {
                        AgentState::Done
                    };
                    return Some(Evidence {
                        state,
                        source: Source::Screen,
                        confidence: Confidence::Low,
                        dismissed_since: Some(status.since_unix_ns),
                    });
                }
            }
            if quiet
                && menu.is_none()
                && self.rules.is_prompt(screen)
                && (self.dialog_waiting_since == Some(status.since_unix_ns)
                    || status.source == Some(Source::Screen))
            {
                self.dialog_waiting_since = None;
                return Some(Evidence {
                    state: AgentState::Done,
                    source: Source::Screen,
                    confidence: Confidence::Low,
                    dismissed_since: Some(status.since_unix_ns),
                });
            }
        } else {
            self.dialog_waiting_since = None;
            self.stray_waiting = None;
        }
        if matches!(status.state, AgentState::Waiting | AgentState::Blocked)
            && status.source == Some(Source::Screen)
            && !quiet
            && title_state != Some(AgentState::Working)
        {
            return None;
        }
        let (state, source, confidence) = if quiet && let Some(state) = menu {
            (state, Source::Screen, Confidence::Low)
        } else if let Some(state) = title_state {
            (state, Source::Title, Confidence::Medium)
        } else if quiet
            && status.source == Some(Source::Screen)
            && matches!(status.state, AgentState::Waiting | AgentState::Blocked)
        {
            (AgentState::Working, Source::Screen, Confidence::Low)
        } else {
            return None;
        };
        Some(Evidence {
            state,
            source,
            confidence,
            dismissed_since: None,
        })
    }

    fn title_state(&mut self, title: &str) -> Option<AgentState> {
        let title = title.trim();
        if title == "π !" || title.starts_with("π ! ") {
            return Some(AgentState::Waiting);
        }
        let (glyph, rest) = (
            title.chars().next()?,
            title.chars().skip(1).collect::<String>(),
        );
        let rules = &self.rules;
        if rest.starts_with(char::is_whitespace) {
            if glyph == rules.done_glyph || glyph == '◇' {
                return Some(AgentState::Done);
            }
            if rules.waiting_glyphs.contains(&glyph) {
                return Some(AgentState::Waiting);
            }
            if rules.working_glyphs.contains(&glyph) {
                return Some(AgentState::Working);
            }
            if rules.spinner_glyphs.contains(&glyph) {
                // Codex도 스피너가 사라진 직후 제목의 본문은 유지한다.
                self.spinning_title = Some(
                    rest.trim_start_matches(|c: char| {
                        c.is_whitespace()
                            || rules.spinner_glyphs.contains(&c)
                            || rules.spinner_separators.contains(&c)
                    })
                    .to_owned(),
                );
                return Some(AgentState::Working);
            }
        }
        if title_indicates_agent_waiting(title) {
            return Some(AgentState::Waiting);
        }
        (self.spinning_title.as_deref() == Some(title) && !title.is_empty())
            .then_some(AgentState::Done)
    }
}

fn title_indicates_agent_waiting(title: &str) -> bool {
    let lower = title.to_ascii_lowercase();
    let has_waiting_keyword = lower.contains("action required") || lower.contains("permission");
    if !has_waiting_keyword {
        return false;
    }
    [
        "claude",
        "codex",
        "agy",
        "antigravity",
        "gemini",
        "cursor",
        "opencode",
        "copilot",
        "aider",
        "kimi",
        "qwen",
        "droid",
        "hermes",
    ]
    .iter()
    .any(|agent| lower.contains(agent))
}

#[cfg(test)]
mod tests;
