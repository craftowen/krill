//! MK-057 Jump 팔레트(Cmd+J) + MK-181 AI Vault 통합 검색:
//! 열린 세션 퍼지 검색(`List`·`States`·`Sidebar`)과 AI Vault 대화 기록(워크스페이스/프로젝트/전체 범위 필터,
//! 에이전트 종류 필터, 스니펫 하이라이트, 원클릭 Resume)을 함께 제공한다.
//!
//! 검색어는 조합 중인 한글(IME preedit)까지 포함해 즉시 거른다: gpui-component `Input`은 조합 중에 `Change`를 내지
//! 않으므로 입력 상태를 직접 관찰한다.

use std::cmp::Reverse;

use gpui::{
    Context, Entity, EventEmitter, Subscription, UniformListScrollHandle, Window, deferred, div,
    prelude::*, px, uniform_list,
};
use gpui_component::input::{Enter, Escape, Input, InputState, MoveDown, MoveUp};
use krill_proto::SessionId;
use krill_proto::agent_history::{VaultAgent, VaultHit, VaultScope};

use crate::i18n::catalog::agent_history as vault_text;
use crate::i18n::catalog::jump as text;

const ROW_HEIGHT: f32 = 28.;
const VISIBLE_ROWS: usize = 12;

/// 팔레트 한 줄. `label`의 소문자 글자를 미리 펼쳐 둬 키 입력마다 할당하지 않는다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub id: SessionId,
    pub label: String,
    pub needs_you: bool,
    chars: Vec<char>,
}

impl Item {
    pub fn new(id: SessionId, label: String, needs_you: bool) -> Self {
        let chars = label.chars().flat_map(char::to_lowercase).collect();
        Self {
            id,
            label,
            needs_you,
            chars,
        }
    }
}

const HANGUL_BASE: u32 = 0xAC00;
const INITIALS: [char; 19] = [
    'ㄱ', 'ㄲ', 'ㄴ', 'ㄷ', 'ㄸ', 'ㄹ', 'ㅁ', 'ㅂ', 'ㅃ', 'ㅅ', 'ㅆ', 'ㅇ', 'ㅈ', 'ㅉ', 'ㅊ', 'ㅋ',
    'ㅌ', 'ㅍ', 'ㅎ',
];

/// 한글 음절의 `0xAC00`부터의 오프셋(초성 × 588 + 중성 × 28 + 종성).
fn syllable(c: char) -> Option<u32> {
    u32::from(c)
        .checked_sub(HANGUL_BASE)
        .filter(|&s| s < 11_172)
}

/// 검색어 글자 `q`가 `c`에 맞는가. 조합 중 글자를 위해 받침 없는 음절은 같은 초성·중성 음절(받침 무관)에,
/// 자음(ㄱ~ㅎ)은 그 초성의 음절에 맞는다(초성 검색 겸용).
// ponytail: 받침이 다음 음절로 넘어가는 순간("한" → "하나")은 한 타 동안 놓친다. 불편하면 자모 분해 비교로.
fn char_matches(q: char, c: char) -> bool {
    if q == c {
        return true;
    }
    let Some(s) = syllable(c) else {
        return false;
    };
    if let Some(qs) = syllable(q) {
        return qs % 28 == 0 && qs / 28 == s / 28;
    }
    INITIALS
        .iter()
        .position(|&i| i == q)
        .and_then(|i| u32::try_from(i).ok())
        .is_some_and(|i| i == s / 588)
}

/// `query`가 `text`의 부분 수열이면 점수(연속 일치·단어 시작에 가점), 아니면 `None`.
// ponytail: 왼쪽부터 탐욕 일치라 뒤쪽의 더 좋은 연속 일치를 놓칠 수 있다. 순위 불만이 나오면 DP(fzf v2식)로.
fn score(query: &[char], text: &[char]) -> Option<u32> {
    let mut total = 0;
    let mut from = 0;
    let mut prev = None;
    for &q in query {
        let i = from + text.get(from..)?.iter().position(|&c| char_matches(q, c))?;
        total += 1;
        if prev.is_some_and(|p: usize| p + 1 == i) {
            total += 4;
        }
        if i.checked_sub(1)
            .and_then(|j| text.get(j))
            .is_none_or(|c| !c.is_alphanumeric())
        {
            total += 2;
        }
        prev = Some(i);
        from = i + 1;
    }
    Some(total)
}

/// 일치하는 항목의 인덱스. 점수 높은 순, 같으면 입력 대기 세션 먼저, 그다음 원래 순서. 공백은 무시한다.
pub fn search(items: &[Item], query: &str) -> Vec<usize> {
    let query: Vec<char> = query
        .chars()
        .filter(|c| !c.is_whitespace())
        .flat_map(char::to_lowercase)
        .collect();
    let mut hits: Vec<_> = items
        .iter()
        .enumerate()
        .filter_map(|(i, item)| {
            score(&query, &item.chars).map(|s| (Reverse(s), !item.needs_you, i))
        })
        .collect();
    hits.sort_unstable();
    hits.into_iter().map(|(_, _, i)| i).collect()
}

fn normalize_path(path: &str) -> String {
    let trimmed = path.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        return String::new();
    }
    if let Some(rest) = trimmed.strip_prefix("/private/tmp")
        && (rest.is_empty() || rest.starts_with('/'))
    {
        return format!("/tmp{rest}");
    }
    if let Some(rest) = trimmed.strip_prefix("/private/var")
        && (rest.is_empty() || rest.starts_with('/'))
    {
        return format!("/var{rest}");
    }
    trimmed.to_owned()
}

fn filter_vault(
    hits: &[VaultHit],
    query: &str,
    scope: VaultScope,
    agent_filter: Option<VaultAgent>,
    workspace_path: Option<&str>,
    project_paths: &[String],
) -> Vec<usize> {
    if hits.is_empty() {
        return Vec::new();
    }
    let active_paths: Vec<String> = match scope {
        VaultScope::All => Vec::new(),
        VaultScope::Project => {
            if project_paths.is_empty() {
                workspace_path.into_iter().map(str::to_owned).collect()
            } else {
                project_paths.to_vec()
            }
        }
        VaultScope::Workspace => workspace_path.into_iter().map(str::to_owned).collect(),
    };
    let terms: Vec<String> = query
        .split_whitespace()
        .filter(|tok| {
            !tok.starts_with("agent:") && !tok.starts_with("path:") && !tok.starts_with("repo:")
        })
        .map(str::to_lowercase)
        .collect();
    hits.iter()
        .enumerate()
        .filter(|(_, hit)| agent_filter.is_none_or(|a| hit.agent == a))
        .filter(|(_, hit)| {
            if active_paths.is_empty() {
                return true;
            }
            let norm = normalize_path(&hit.cwd);
            active_paths.iter().any(|p| {
                let p_norm = normalize_path(p);
                norm == p_norm
                    || norm
                        .strip_prefix(p_norm.as_str())
                        .is_some_and(|rest| rest.starts_with('/'))
            })
        })
        .filter(|(_, hit)| {
            if terms.is_empty() {
                return true;
            }
            let hay = format!(
                "{} {} {} {} {}",
                hit.title.to_lowercase(),
                hit.summary.to_lowercase(),
                hit.snippet.to_lowercase(),
                hit.cwd.to_lowercase(),
                hit.id.to_lowercase()
            );
            terms.iter().all(|t| hay.contains(t))
        })
        .map(|(i, _)| i)
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    Confirm(SessionId),
    ResumeVault(usize),
    Dismiss,
}

pub struct Palette {
    language: crate::i18n::Language,
    items: Vec<Item>,
    matches: Vec<usize>,
    vault_hits: Vec<VaultHit>,
    vault_matches: Vec<usize>,
    scope: VaultScope,
    agent_filter: Option<VaultAgent>,
    workspace_path: Option<String>,
    project_paths: Vec<String>,
    selected: usize,
    query: String,
    input: Entity<InputState>,
    scroll: UniformListScrollHandle,
    _observe: Subscription,
}

impl EventEmitter<Event> for Palette {}

impl std::fmt::Debug for Palette {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Palette")
            .field("items", &self.items.len())
            .field("matches", &self.matches.len())
            .field("vault_hits", &self.vault_hits.len())
            .field("selected", &self.selected)
            .finish_non_exhaustive()
    }
}

impl Palette {
    pub fn new(items: Vec<Item>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder(text::PLACEHOLDER.get()));
        // 조합 중 글자도 값에 들어 있다. 커서 깜박임 같은 알림은 값이 같아 건너뛴다.
        let observe = cx.observe(&input, |p, input, cx| {
            let value = input.read(cx).value();
            if *value != p.query {
                p.query = value.to_string();
                p.matches = search(&p.items, &p.query);
                p.recompute_vault_matches();
                p.selected = 0;
                p.scroll.scroll_to_item(0, gpui::ScrollStrategy::Top);
                cx.notify();
            }
        });
        Self {
            language: crate::i18n::language(),
            matches: search(&items, ""),
            items,
            vault_hits: Vec::new(),
            vault_matches: Vec::new(),
            scope: VaultScope::All,
            agent_filter: None,
            workspace_path: None,
            project_paths: Vec::new(),
            selected: 0,
            query: String::new(),
            input,
            scroll: UniformListScrollHandle::new(),
            _observe: observe,
        }
    }

    pub fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.input.update(cx, |input, cx| input.focus(window, cx));
    }

    /// 지금 보이는 순서의 일치 항목.
    pub fn matches(&self) -> impl Iterator<Item = &Item> {
        self.matches.iter().filter_map(|&i| self.items.get(i))
    }

    /// 지금 필터에 일치하는 AI Vault 항목.
    pub fn vault_matches(&self) -> impl Iterator<Item = &VaultHit> {
        self.vault_matches
            .iter()
            .filter_map(|&i| self.vault_hits.get(i))
    }

    #[must_use]
    pub fn vault_hit(&self, index: usize) -> Option<&VaultHit> {
        self.vault_hits.get(index)
    }

    #[must_use]
    pub fn scope(&self) -> VaultScope {
        self.scope
    }

    #[must_use]
    pub fn agent_filter(&self) -> Option<VaultAgent> {
        self.agent_filter
    }

    pub fn set_vault_hits(&mut self, hits: Vec<VaultHit>, cx: &mut Context<Self>) {
        self.vault_hits = hits;
        self.recompute_vault_matches();
        cx.notify();
    }

    pub fn set_scope(&mut self, scope: VaultScope, cx: &mut Context<Self>) {
        if self.scope != scope {
            self.scope = scope;
            self.recompute_vault_matches();
            cx.notify();
        }
    }

    pub fn set_agent_filter(&mut self, agent: Option<VaultAgent>, cx: &mut Context<Self>) {
        if self.agent_filter != agent {
            self.agent_filter = agent;
            self.recompute_vault_matches();
            cx.notify();
        }
    }

    pub fn set_scope_paths(
        &mut self,
        workspace: Option<String>,
        project: Vec<String>,
        cx: &mut Context<Self>,
    ) {
        self.workspace_path = workspace;
        self.project_paths = project;
        self.recompute_vault_matches();
        cx.notify();
    }

    pub fn resume_vault_at(&mut self, hit_index: usize, cx: &mut Context<Self>) {
        if self.vault_hits.get(hit_index).is_some() {
            cx.emit(Event::ResumeVault(hit_index));
        }
    }

    fn recompute_vault_matches(&mut self) {
        self.vault_matches = filter_vault(
            &self.vault_hits,
            &self.query,
            self.scope,
            self.agent_filter,
            self.workspace_path.as_deref(),
            &self.project_paths,
        );
    }

    pub fn set_items(&mut self, items: Vec<Item>, cx: &mut Context<Self>) {
        let selected = self.matches().nth(self.selected).map(|item| item.id);
        self.items = items;
        self.matches = search(&self.items, &self.query);
        let selected_index = self
            .matches()
            .position(|item| Some(item.id) == selected)
            .unwrap_or(0);
        self.selected = selected_index;
        cx.notify();
    }

    pub fn input(&self) -> &Entity<InputState> {
        &self.input
    }

    fn step(&mut self, delta: isize, cx: &mut Context<Self>) {
        let last = self.matches.len().saturating_sub(1);
        self.selected = self.selected.saturating_add_signed(delta).min(last);
        self.scroll
            .scroll_to_item(self.selected, gpui::ScrollStrategy::Center);
        cx.notify();
    }

    fn confirm(&mut self, cx: &mut Context<Self>) {
        if let Some(item) = self.matches().nth(self.selected) {
            cx.emit(Event::Confirm(item.id));
        } else if let Some(&vault_idx) = self.vault_matches.first() {
            cx.emit(Event::ResumeVault(vault_idx));
        }
    }
}

fn render_vault_highlight(text_str: &str, query: &str, p: &crate::ui::Palette) -> gpui::Div {
    let mut row = div().flex().flex_wrap().items_center();
    for (seg, is_hit) in crate::agent_history::highlight_spans(text_str, query) {
        if is_hit {
            row = row.child(
                div()
                    .px_0p5()
                    .rounded(crate::ui::RADIUS_XS)
                    .bg(p.accent)
                    .text_color(p.accent_foreground)
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child(seg),
            );
        } else {
            row = row.child(div().child(seg));
        }
    }
    row
}

impl Render for Palette {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.language != crate::i18n::language() {
            self.language = crate::i18n::language();
            self.input.update(cx, |input, cx| {
                input.set_placeholder(text::PLACEHOLDER.get(), window, cx);
            });
        }
        let pal = crate::ui::palette(cx);
        let (bg, fg, border, accent, warning, muted) = (
            pal.popover,
            pal.popover_foreground,
            pal.border,
            pal.accent,
            pal.agent_question,
            pal.muted_foreground,
        );
        let rows = self.matches.len();
        #[expect(
            clippy::cast_precision_loss,
            reason = "VISIBLE_ROWS 이하라 정확히 표현된다"
        )]
        let height = rows.min(VISIBLE_ROWS) as f32 * ROW_HEIGHT;
        let list = uniform_list(
            "jump-list",
            rows,
            cx.processor(move |p, range: std::ops::Range<usize>, _, cx| {
                range
                    .filter_map(|i| {
                        let item = p.items.get(*p.matches.get(i)?)?;
                        Some(
                            div()
                                .id(i)
                                .h(px(ROW_HEIGHT))
                                .px_2()
                                .rounded(crate::ui::RADIUS_XS)
                                .flex()
                                .items_center()
                                .gap_1p5()
                                .overflow_hidden()
                                .cursor_pointer()
                                .text_size(crate::ui::TEXT_SM)
                                .text_color(fg)
                                .when(i == p.selected, |d| d.bg(accent))
                                .child(
                                    div()
                                        .text_size(crate::ui::TEXT_2XS)
                                        .text_color(warning)
                                        .when(!item.needs_you, Styled::invisible)
                                        .child("●"),
                                )
                                .child(div().flex_1().truncate().child(item.label.clone()))
                                .on_click(cx.listener(move |p, _, _, cx| {
                                    p.selected = i;
                                    p.confirm(cx);
                                })),
                        )
                    })
                    .collect()
            }),
        )
        .track_scroll(&self.scroll)
        .h(px(height));

        let vault_section = (!self.vault_hits.is_empty()).then(|| {
            let mut filter_bar = div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap_1()
                .pt_1()
                .border_t_1()
                .border_color(border)
                .text_size(crate::ui::TEXT_XS);
            for (id, scope, label) in [
                (
                    "jump-scope-all",
                    VaultScope::All,
                    vault_text::SCOPE_ALL.get(),
                ),
                (
                    "jump-scope-project",
                    VaultScope::Project,
                    vault_text::SCOPE_PROJECT.get(),
                ),
                (
                    "jump-scope-workspace",
                    VaultScope::Workspace,
                    vault_text::SCOPE_WORKSPACE.get(),
                ),
            ] {
                let active = self.scope == scope;
                filter_bar = filter_bar.child(
                    div()
                        .id(id)
                        .debug_selector(move || id.into())
                        .px_1p5()
                        .py_0p5()
                        .rounded(crate::ui::RADIUS_XS)
                        .cursor_pointer()
                        .bg(if active { pal.accent } else { pal.secondary })
                        .text_color(if active {
                            pal.accent_foreground
                        } else {
                            pal.secondary_foreground
                        })
                        .child(label)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.set_scope(scope, cx);
                        })),
                );
            }
            for (id, agent, label) in [
                ("jump-agent-all", None, vault_text::AGENT_ALL.get()),
                ("jump-agent-claude", Some(VaultAgent::Claude), "Claude"),
                ("jump-agent-codex", Some(VaultAgent::Codex), "Codex"),
                ("jump-agent-agy", Some(VaultAgent::Agy), "Antigravity"),
                (
                    "jump-agent-opencode",
                    Some(VaultAgent::OpenCode),
                    "OpenCode",
                ),
            ] {
                let active = self.agent_filter == agent;
                filter_bar = filter_bar.child(
                    div()
                        .id(id)
                        .debug_selector(move || id.into())
                        .px_1p5()
                        .py_0p5()
                        .rounded(crate::ui::RADIUS_XS)
                        .cursor_pointer()
                        .bg(if active { pal.accent } else { pal.secondary })
                        .text_color(if active {
                            pal.accent_foreground
                        } else {
                            pal.secondary_foreground
                        })
                        .child(label)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.set_agent_filter(agent, cx);
                        })),
                );
            }

            let mut rows_div = div()
                .flex()
                .flex_col()
                .gap_1()
                .text_size(crate::ui::TEXT_XS);
            for &hit_idx in self.vault_matches.iter().take(6) {
                if let Some(hit) = self.vault_hits.get(hit_idx) {
                    let title = if hit.title.is_empty() {
                        format!("{} · {}", hit.agent.profile(), hit.id)
                    } else {
                        format!("{} · {}", hit.agent.profile(), hit.title)
                    };
                    let mut col = div()
                        .flex_1()
                        .flex()
                        .flex_col()
                        .overflow_hidden()
                        .child(render_vault_highlight(&title, &self.query, pal));
                    if !hit.snippet.is_empty() {
                        col = col.child(div().text_color(muted).child(render_vault_highlight(
                            &hit.snippet,
                            &self.query,
                            pal,
                        )));
                    }
                    rows_div = rows_div.child(
                        div()
                            .id(("jump-vault-row", hit_idx))
                            .px_2()
                            .py_1()
                            .rounded(crate::ui::RADIUS_XS)
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(col)
                            .child(
                                div()
                                    .id(("jump-vault-resume", hit_idx))
                                    .debug_selector(move || format!("jump-vault-resume-{hit_idx}"))
                                    .px_2()
                                    .py_0p5()
                                    .rounded(crate::ui::RADIUS_XS)
                                    .bg(pal.secondary)
                                    .text_color(pal.secondary_foreground)
                                    .cursor_pointer()
                                    .child(vault_text::RESUME.get())
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.resume_vault_at(hit_idx, cx);
                                    })),
                            ),
                    );
                }
            }
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(filter_bar)
                .child(rows_div)
        });

        // 키는 입력칸보다 먼저(capture) 잡아 단일 행 입력의 위·아래·Enter·Esc 처리를 대신한다.
        let palette = div()
            .w(crate::ui::COMMAND_PALETTE_WIDTH)
            .p_2()
            .flex()
            .flex_col()
            .gap_1p5()
            .bg(bg)
            .text_color(fg)
            .border_1()
            .border_color(border)
            .rounded(crate::ui::RADIUS_LG)
            .shadow_lg()
            .capture_action(cx.listener(|p, _: &MoveUp, _, cx| {
                cx.stop_propagation();
                p.step(-1, cx);
            }))
            .capture_action(cx.listener(|p, _: &MoveDown, _, cx| {
                cx.stop_propagation();
                p.step(1, cx);
            }))
            .capture_action(cx.listener(|p, _: &Enter, _, cx| {
                cx.stop_propagation();
                p.confirm(cx);
            }))
            .capture_action(cx.listener(|_, _: &Escape, _, cx| {
                cx.stop_propagation();
                cx.emit(Event::Dismiss);
            }))
            .on_mouse_down_out(cx.listener(|_, _, _, cx| cx.emit(Event::Dismiss)))
            .child(Input::new(&self.input))
            .when(rows == 0 && self.vault_matches.is_empty(), |d| {
                d.child(
                    div()
                        .p_2()
                        .text_size(crate::ui::TEXT_XS)
                        .text_color(muted)
                        .child(text::EMPTY.get()),
                )
            })
            .child(list)
            .children(vault_section);
        deferred(
            div()
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .flex()
                .justify_center()
                .pt(px(80.))
                .child(palette),
        )
    }
}

#[cfg(test)]
#[path = "jump/tests.rs"]
mod tests;
