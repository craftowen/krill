//! Branch selection adapted from Orca `ComposerBaseRefPicker`/`CreateFromPicker`.
//! Copyright (c) 2026 Lovecast Inc. (MIT License; docs/licensing/spdx/MIT.txt).
use super::*;

/// 실제로 관측한 부모·저장소 브랜치를 앞에 두고 git refs를 중복 없이 합친다.
/// 기본 브랜치 이름을 추측하지 않는다. 정보가 없으면 데몬의 기본 ref 결정을 따른다.
pub fn branch_suggestions(
    parent_branch: Option<&str>,
    repo_branch: Option<&str>,
    git_refs: &[String],
) -> (Option<String>, Vec<String>) {
    let mut out = Vec::new();
    for raw in parent_branch
        .into_iter()
        .chain(repo_branch)
        .chain(git_refs.iter().map(String::as_str))
    {
        let value = raw.trim();
        if !value.is_empty()
            && !value.chars().any(char::is_control)
            && !out.iter().any(|item| item == value)
        {
            out.push(value.to_owned());
        }
    }
    let default = parent_branch
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned);
    (default, out)
}

/// OWE-853: 로컬 `git for-each-ref`로 브랜치·태그를 읽는다(새 IPC 없이).
/// 실패·타임아웃 시 빈 목록으로 조용히 넘어간다. 호출자는 백그라운드에서 부른다.
pub fn list_git_refs(repo: &Path) -> Vec<String> {
    let mut command = crate::platform::background_command("git");
    command
        .current_dir(repo)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .args([
            "for-each-ref",
            "--format=%(refname:short)",
            "refs/heads",
            "refs/remotes",
            "refs/tags",
        ]);
    let output =
        crate::pull_request::execute(&mut command, Duration::from_millis(1500)).unwrap_or_default();
    let text = String::from_utf8_lossy(&output);
    let mut out = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty()
            || trimmed.len() > 256
            || trimmed.chars().any(|c| c.is_control() || c.is_whitespace())
        {
            continue;
        }
        if !out.iter().any(|e: &String| e == trimmed) {
            out.push(trimmed.to_owned());
        }
    }
    out
}

impl Dialog {
    pub(super) fn apply_repo_branch_options(
        &mut self,
        repo_branch: Option<&str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (default_base, initial_candidates) = branch_suggestions(None, repo_branch, &[]);
        self.set_branch_candidates(initial_candidates, cx);
        let default_str = default_base.unwrap_or_default();
        self.base.update(cx, |input, cx| {
            input.set_value(&default_str, window, cx);
        });
        self.close_branch_picker(window, cx);
        self.load_branches(None, repo_branch.map(str::to_owned), cx);
    }

    /// Each load owns a generation: A → B → A must reject the first A result too.
    pub fn load_branches(
        &mut self,
        parent_branch: Option<String>,
        repo_branch: Option<String>,
        cx: &mut Context<Self>,
    ) {
        self.branch_generation = self.branch_generation.wrapping_add(1);
        self.branch_task = None;
        self.branch_error = None;
        let generation = self.branch_generation;
        let repo = self.repo.clone();
        let source = self.source.clone();
        let local = self.host.is_local();
        self.branch_task = Some(cx.spawn(async move |this, cx| {
            let refs = cx
                .background_executor()
                .spawn(async move {
                    match source.worktree_refs(&repo) {
                        Err(error) if local && error.kind() == std::io::ErrorKind::Unsupported => {
                            Ok(list_git_refs(&repo))
                        }
                        result => result,
                    }
                })
                .await;
            let error = refs.as_ref().err().map(std::io::Error::kind);
            let refs = refs.unwrap_or_default();
            let (_, candidates) =
                branch_suggestions(parent_branch.as_deref(), repo_branch.as_deref(), &refs);
            this.update(cx, |dialog, cx| {
                dialog.finish_branches(generation, candidates, error, cx);
            })
            .ok();
        }));
    }

    pub(super) fn finish_branches(
        &mut self,
        generation: u64,
        candidates: Vec<String>,
        error: Option<std::io::ErrorKind>,
        cx: &mut Context<Self>,
    ) {
        if self.branch_generation == generation {
            self.branch_error = error;
            self.set_branch_candidates(candidates, cx);
        }
    }

    /// 실제 후보를 교체한다. 검색은 전체 목록에 적용하고 렌더는 가시 행만 한다.
    pub fn set_branch_candidates(&mut self, candidates: Vec<String>, cx: &mut Context<Self>) {
        let mut seen = std::collections::BTreeSet::new();
        let mut cleaned = Vec::new();
        for raw in candidates {
            let trimmed = raw.trim().to_owned();
            if trimmed.is_empty()
                || trimmed.len() > 256
                || trimmed.chars().any(char::is_control)
                || !seen.insert(trimmed.clone())
            {
                continue;
            }
            cleaned.push(trimmed);
        }
        let highlighted = self
            .highlight
            .and_then(|index| self.filtered_bases(cx).get(index).cloned());
        self.candidates = cleaned;
        self.highlight = self.branch_picker_open.then(|| {
            highlighted
                .and_then(|value| {
                    self.filtered_bases(cx)
                        .iter()
                        .position(|candidate| *candidate == value)
                })
                .unwrap_or(0)
        });
        cx.notify();
    }

    pub(super) fn filtered_bases(&self, cx: &App) -> Vec<String> {
        let query = self.branch_query.read(cx).value().trim().to_lowercase();
        let mut candidates = Vec::new();
        // The empty value is the project-default choice, never a fabricated Git ref.
        if text::DEFAULT_BRANCH.get().to_lowercase().contains(&query) {
            candidates.push(String::new());
        }
        candidates.extend(
            self.candidates
                .iter()
                .filter(|candidate| candidate.to_lowercase().contains(query.as_str()))
                .cloned(),
        );
        candidates
    }

    pub(super) fn open_branch_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.branch_picker_open = true;
        self.highlight = Some(0);
        self.branch_query.update(cx, |input, cx| {
            input.set_value("", window, cx);
            input.focus(window, cx);
        });
        self.branch_scroll
            .scroll_to_item(0, gpui::ScrollStrategy::Top);
        cx.notify();
    }

    pub(super) fn close_branch_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.branch_picker_open {
            self.branch_picker_open = false;
            self.highlight = None;
            self.name.update(cx, |input, cx| input.focus(window, cx));
            cx.notify();
        }
    }

    pub(super) fn pick_base(&mut self, picked: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.base
            .update(cx, |input, cx| input.set_value(picked, window, cx));
        self.close_branch_picker(window, cx);
        self.prepare(cx);
        cx.notify();
    }

    /// IME owns confirmation and cancellation while any focused text field is composing.
    pub(super) fn input_is_composing(&self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        [
            &self.name,
            &self.branch_query,
            &self.base,
            &self.initial_prompt,
        ]
        .into_iter()
        .any(|input| {
            input.update(cx, |input, cx| {
                input.focus_handle(cx).is_focused(window)
                    && gpui::EntityInputHandler::marked_text_range(input, window, cx).is_some()
            })
        })
    }

    pub(super) fn handle_branch_key(
        &mut self,
        key: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.branch_picker_open || self.input_is_composing(window, cx) {
            return false;
        }
        match key {
            "escape" | "tab" => {
                self.close_branch_picker(window, cx);
                true
            }
            "enter" => {
                if let Some(picked) = self
                    .filtered_bases(cx)
                    .get(self.highlight.unwrap_or(0))
                    .cloned()
                {
                    self.pick_base(&picked, window, cx);
                }
                true
            }
            "down" | "up" => {
                let count = self.filtered_bases(cx).len();
                if count > 0 {
                    let current = self.highlight.unwrap_or(0).min(count - 1);
                    let next = if key == "down" {
                        (current + 1) % count
                    } else if current == 0 {
                        count - 1
                    } else {
                        current - 1
                    };
                    self.highlight = Some(next);
                    self.branch_scroll
                        .scroll_to_item(next, gpui::ScrollStrategy::Nearest);
                    cx.notify();
                }
                true
            }
            _ => false,
        }
    }

    pub(super) fn on_key(
        &mut self,
        e: &gpui::KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if !matches!(self.phase, Phase::Edit(_)) {
            return false;
        }
        if self.input_is_composing(window, cx) {
            return false;
        }
        if self.handle_branch_key(e.keystroke.key.as_str(), window, cx) {
            return true;
        }
        if e.keystroke.key == "enter"
            && (e.keystroke.modifiers.platform
                || (!cfg!(target_os = "macos") && e.keystroke.modifiers.control))
        {
            self.submit(Setup::Ask, cx);
            return true;
        }
        if e.keystroke.key == "enter" && self.name.read(cx).focus_handle(cx).is_focused(window) {
            // Orca handleNamePlainEnter advances to the quick-agent control.
            window.focus_next(cx);
            return true;
        }
        false
    }

    #[cfg(test)]
    pub(crate) fn base_value(&self, cx: &App) -> String {
        self.base.read(cx).value().to_string()
    }
}
