use std::ops::Range;
use std::sync::{Arc, OnceLock};

use krill_app::i18n::catalog::chat as ctext;
use krill_app::ui;
use krill_proto::agent_history::{ChatItem, ChatItemKind};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct RawSpan {
    start: usize,
    end: usize,
}

impl RawSpan {
    pub(super) fn single(abs_idx: usize) -> Self {
        Self {
            start: abs_idx,
            end: abs_idx + 1,
        }
    }

    pub(super) fn start(self) -> usize {
        self.start
    }

    pub(super) fn end(self) -> usize {
        self.end
    }

    pub(super) fn range(self) -> Range<usize> {
        self.start..self.end
    }
}

#[derive(Debug, Default)]
pub(super) struct DisplayFeed {
    rows: Vec<DisplayRow>,
}

impl DisplayFeed {
    pub(super) fn empty() -> Arc<Self> {
        static EMPTY: OnceLock<Arc<DisplayFeed>> = OnceLock::new();
        Arc::clone(EMPTY.get_or_init(|| Arc::new(Self::default())))
    }

    pub(super) fn project(base_index: usize, items: &[Arc<ChatItem>]) -> Self {
        let mut rows = Vec::new();
        let mut rel_idx = 0;
        while rel_idx < items.len() {
            let item = &items[rel_idx];
            if !is_work_kind(item.kind) {
                let abs_idx = base_index + rel_idx;
                rows.push(DisplayRow::Item {
                    abs_idx,
                    item: Arc::clone(item),
                });
                rel_idx += 1;
                continue;
            }
            let mut rel_end = rel_idx + 1;
            while rel_end < items.len() && is_work_kind(items[rel_end].kind) {
                rel_end += 1;
            }
            let slice = &items[rel_idx..rel_end];
            let has_tool = slice.iter().any(|entry| {
                matches!(
                    entry.kind,
                    ChatItemKind::ToolCall | ChatItemKind::ToolResult
                )
            });
            let start_abs = base_index + rel_idx;
            if !has_tool && slice.len() == 1 {
                rows.push(DisplayRow::Item {
                    abs_idx: start_abs,
                    item: Arc::clone(&slice[0]),
                });
            } else {
                let span = RawSpan {
                    start: start_abs,
                    end: base_index + rel_end,
                };
                rows.push(DisplayRow::WorkRun(WorkRun::from_slice(span, slice)));
            }
            rel_idx = rel_end;
        }
        Self { rows }
    }

    pub(super) fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    pub(super) fn len(&self) -> usize {
        self.rows.len()
    }

    pub(super) fn get(&self, row_ix: usize) -> Option<&DisplayRow> {
        self.rows.get(row_ix)
    }

    pub(super) fn rows(&self) -> &[DisplayRow] {
        &self.rows
    }
}

#[derive(Clone, Debug)]
pub(super) enum DisplayRow {
    Item { abs_idx: usize, item: Arc<ChatItem> },
    WorkRun(WorkRun),
}

impl DisplayRow {
    pub(super) fn span(&self) -> RawSpan {
        match self {
            Self::Item { abs_idx, .. } => RawSpan::single(*abs_idx),
            Self::WorkRun(run) => run.span,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct WorkCounts {
    pub read: usize,
    pub edit: usize,
    pub search: usize,
    pub command: usize,
    pub other: usize,
    pub reasoning: usize,
    pub tool_calls: usize,
    pub errors: usize,
}

#[derive(Clone, Debug)]
pub(super) struct WorkRun {
    pub span: RawSpan,
    pub steps: Vec<WorkStep>,
    pub counts: WorkCounts,
    pub latest_detail: String,
}

impl WorkRun {
    fn from_slice(span: RawSpan, slice: &[Arc<ChatItem>]) -> Self {
        let mut steps: Vec<WorkStep> = Vec::with_capacity(slice.len());
        let mut counts = WorkCounts::default();
        let mut tool_detail = None;
        let mut thought_detail = None;

        for (offset, item) in slice.iter().enumerate() {
            let abs_idx = span.start() + offset;
            match item.kind {
                ChatItemKind::ToolCall => {
                    counts.tool_calls += 1;
                    match ToolStepKind::from_name_and_title(&item.tool_name, &item.title) {
                        ToolStepKind::Read => counts.read += 1,
                        ToolStepKind::Edit => counts.edit += 1,
                        ToolStepKind::Search => counts.search += 1,
                        ToolStepKind::Command => counts.command += 1,
                        ToolStepKind::Other => counts.other += 1,
                    }
                    if item.is_error {
                        counts.errors += 1;
                    }
                    let detail = call_detail(item);
                    if !detail.is_empty() {
                        tool_detail = Some(detail);
                    }
                    steps.push(WorkStep::Tool(ToolPair::CallOnly {
                        call_abs_idx: abs_idx,
                        call: Arc::clone(item),
                    }));
                }
                ChatItemKind::ToolResult => {
                    let matched_step = if item.tool_id.is_empty() {
                        steps
                            .iter()
                            .position(|step| {
                                matches!(
                                    step,
                                    WorkStep::Tool(ToolPair::CallOnly { call, .. })
                                        if call.tool_id.is_empty()
                                )
                            })
                            .or_else(|| {
                                steps.iter().position(|step| {
                                    matches!(step, WorkStep::Tool(ToolPair::CallOnly { .. }))
                                })
                            })
                    } else {
                        steps.iter().position(|step| {
                            matches!(
                                step,
                                WorkStep::Tool(ToolPair::CallOnly { call, .. })
                                    if call.tool_id == item.tool_id
                            )
                        })
                    };

                    if let Some(step_idx) = matched_step
                        && let Some(WorkStep::Tool(pair)) = steps.get_mut(step_idx)
                        && let ToolPair::CallOnly { call_abs_idx, call } = pair
                    {
                        let call_abs_idx = *call_abs_idx;
                        let call = Arc::clone(call);
                        if item.is_error && !call.is_error {
                            counts.errors += 1;
                        }
                        *pair = ToolPair::CallAndResult {
                            call_abs_idx,
                            call,
                            result_abs_idx: abs_idx,
                            result: Arc::clone(item),
                        };
                    } else {
                        if item.is_error {
                            counts.errors += 1;
                        }
                        let detail = orphan_result_detail(item);
                        if !detail.is_empty() {
                            tool_detail = Some(detail);
                        }
                        steps.push(WorkStep::Tool(ToolPair::OrphanResult {
                            result_abs_idx: abs_idx,
                            result: Arc::clone(item),
                        }));
                    }
                }
                ChatItemKind::Reasoning => {
                    counts.reasoning += 1;
                    let detail = first_non_empty_line(&item.text);
                    if !detail.is_empty() {
                        thought_detail = Some(detail);
                    }
                    steps.push(WorkStep::Reasoning {
                        abs_idx,
                        item: Arc::clone(item),
                    });
                }
                _ => {}
            }
        }

        Self {
            span,
            steps,
            counts,
            latest_detail: tool_detail.or(thought_detail).unwrap_or("").to_owned(),
        }
    }

    pub(super) fn summary_label(&self) -> String {
        if self.counts.tool_calls == 0 {
            if self.counts.reasoning > 0 {
                return ctext::RUN_REASONING.format(&[("n", &self.counts.reasoning.to_string())]);
            }
            return ctext::RUN_ACTIVITY.get().to_owned();
        }
        let mut parts = Vec::with_capacity(5);
        for (count, message) in [
            (self.counts.read, ctext::RUN_READ),
            (self.counts.edit, ctext::RUN_EDIT),
            (self.counts.search, ctext::RUN_SEARCH),
            (self.counts.command, ctext::RUN_COMMAND),
            (self.counts.other, ctext::RUN_OTHER),
        ] {
            if count > 0 {
                parts.push(message.format(&[("n", &count.to_string())]));
            }
        }
        if parts.is_empty() {
            ctext::RUN_ACTIVITY.get().to_owned()
        } else {
            parts.join(" · ")
        }
    }
}

#[derive(Clone, Debug)]
pub(super) enum ToolPair {
    CallOnly {
        call_abs_idx: usize,
        call: Arc<ChatItem>,
    },
    CallAndResult {
        call_abs_idx: usize,
        call: Arc<ChatItem>,
        result_abs_idx: usize,
        result: Arc<ChatItem>,
    },
    OrphanResult {
        result_abs_idx: usize,
        result: Arc<ChatItem>,
    },
}

#[derive(Clone, Debug)]
pub(super) enum WorkStep {
    Tool(ToolPair),
    Reasoning { abs_idx: usize, item: Arc<ChatItem> },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct StepBodySection<'a> {
    pub abs_idx: usize,
    pub label: Option<&'static str>,
    pub text: &'a str,
}

impl WorkStep {
    pub(super) fn primary_abs_idx(&self) -> usize {
        match self {
            Self::Tool(
                ToolPair::CallOnly { call_abs_idx, .. }
                | ToolPair::CallAndResult { call_abs_idx, .. },
            ) => *call_abs_idx,
            Self::Tool(ToolPair::OrphanResult { result_abs_idx, .. }) => *result_abs_idx,
            Self::Reasoning { abs_idx, .. } => *abs_idx,
        }
    }

    pub(super) fn is_error(&self) -> bool {
        match self {
            Self::Tool(ToolPair::CallOnly { call, .. }) => call.is_error,
            Self::Tool(ToolPair::CallAndResult { call, result, .. }) => {
                call.is_error || result.is_error
            }
            Self::Tool(ToolPair::OrphanResult { result, .. }) => result.is_error,
            Self::Reasoning { .. } => false,
        }
    }

    pub(super) fn badge(&self) -> (ui::BadgeTone, &'static str) {
        if self.is_error() {
            return (ui::BadgeTone::Destructive, ctext::TOOL_ERROR.get());
        }
        match self {
            Self::Tool(ToolPair::CallOnly { call, .. } | ToolPair::CallAndResult { call, .. }) => {
                let kind = ToolStepKind::from_name_and_title(&call.tool_name, &call.title);
                (kind.tone(), kind.label())
            }
            Self::Tool(ToolPair::OrphanResult { result, .. }) => {
                let kind = ToolStepKind::from_name_and_title(&result.tool_name, &result.title);
                (kind.tone(), kind.label())
            }
            Self::Reasoning { .. } => (ui::BadgeTone::Muted, ctext::REASONING.get()),
        }
    }

    pub(super) fn headline(&self) -> String {
        match self {
            Self::Tool(ToolPair::CallOnly { call, .. }) => {
                let detail = call_detail(call);
                if detail.is_empty() {
                    ctext::TOOL_CALL.get().to_owned()
                } else {
                    detail.to_owned()
                }
            }
            Self::Tool(ToolPair::CallAndResult { call, result, .. }) => {
                let detail = call_detail(call);
                if detail.is_empty() {
                    let fallback = orphan_result_detail(result);
                    if fallback.is_empty() {
                        ctext::TOOL_CALL.get().to_owned()
                    } else {
                        fallback.to_owned()
                    }
                } else {
                    detail.to_owned()
                }
            }
            Self::Tool(ToolPair::OrphanResult { result, .. }) => {
                let title = first_non_empty_line(&result.title);
                if title.is_empty() {
                    let detail = orphan_result_detail(result);
                    if detail.is_empty() {
                        ctext::TOOL_RESULT.get().to_owned()
                    } else {
                        detail.to_owned()
                    }
                } else {
                    title.to_owned()
                }
            }
            Self::Reasoning { item, .. } => {
                let line = first_non_empty_line(&item.text);
                if line.is_empty() {
                    let title = first_non_empty_line(&item.title);
                    if title.is_empty() {
                        ctext::REASONING.get().to_owned()
                    } else {
                        title.to_owned()
                    }
                } else {
                    line.to_owned()
                }
            }
        }
    }

    pub(super) fn file_path(&self) -> &str {
        match self {
            Self::Tool(ToolPair::CallOnly { call, .. }) => call.file_path.trim(),
            Self::Tool(ToolPair::CallAndResult { call, result, .. }) => {
                let call_path = call.file_path.trim();
                if call_path.is_empty() {
                    result.file_path.trim()
                } else {
                    call_path
                }
            }
            Self::Tool(ToolPair::OrphanResult { result, .. }) => result.file_path.trim(),
            Self::Reasoning { .. } => "",
        }
    }

    pub(super) fn body_sections(&self) -> Vec<StepBodySection<'_>> {
        match self {
            Self::Tool(ToolPair::CallOnly { call_abs_idx, call }) => {
                if call.text.trim().is_empty() {
                    Vec::new()
                } else {
                    vec![StepBodySection {
                        abs_idx: *call_abs_idx,
                        label: Some(ctext::STEP_INPUT.get()),
                        text: &call.text,
                    }]
                }
            }
            Self::Tool(ToolPair::CallAndResult {
                call_abs_idx,
                call,
                result_abs_idx,
                result,
            }) => {
                let mut sections = Vec::with_capacity(2);
                let call_trimmed = call.text.trim();
                let result_trimmed = result.text.trim();
                let headline = self.headline();
                let suppress_input = !result_trimmed.is_empty()
                    && (call_trimmed.is_empty()
                        || call_trimmed == call.file_path.trim()
                        || call_trimmed == headline);
                if !call_trimmed.is_empty() && !suppress_input {
                    sections.push(StepBodySection {
                        abs_idx: *call_abs_idx,
                        label: Some(ctext::STEP_INPUT.get()),
                        text: &call.text,
                    });
                }
                if !result_trimmed.is_empty() {
                    sections.push(StepBodySection {
                        abs_idx: *result_abs_idx,
                        label: Some(if result.is_error {
                            ctext::TOOL_ERROR.get()
                        } else {
                            ctext::STEP_OUTPUT.get()
                        }),
                        text: &result.text,
                    });
                }
                sections
            }
            Self::Tool(ToolPair::OrphanResult {
                result_abs_idx,
                result,
            }) => {
                if result.text.trim().is_empty() {
                    Vec::new()
                } else {
                    vec![StepBodySection {
                        abs_idx: *result_abs_idx,
                        label: None,
                        text: &result.text,
                    }]
                }
            }
            Self::Reasoning { abs_idx, item } => {
                if item.text.trim().is_empty() {
                    Vec::new()
                } else {
                    vec![StepBodySection {
                        abs_idx: *abs_idx,
                        label: None,
                        text: &item.text,
                    }]
                }
            }
        }
    }

    pub(super) fn copy_text(&self) -> String {
        let mut parts = Vec::new();
        for section in self.body_sections() {
            let cleaned = krill_term::select::clean_text(section.text);
            if !cleaned.is_empty() {
                parts.push(cleaned);
            }
        }
        parts.join("\n\n")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ToolStepKind {
    Read,
    Edit,
    Search,
    Command,
    Other,
}

impl ToolStepKind {
    fn from_name_and_title(name: &str, title: &str) -> Self {
        let n = name.to_ascii_lowercase();
        let t = title.to_ascii_lowercase();
        if n.contains("read")
            || n.contains("view")
            || n == "cat"
            || t.contains("read")
            || t.contains("view")
        {
            Self::Read
        } else if n.contains("edit")
            || n.contains("write")
            || n.contains("patch")
            || t.contains("edit")
            || t.contains("write")
        {
            Self::Edit
        } else if n.contains("grep")
            || n.contains("glob")
            || n.contains("search")
            || n.contains("find")
            || n == "ls"
            || t.contains("search")
            || t.contains("grep")
        {
            Self::Search
        } else if n.contains("bash")
            || n.contains("shell")
            || n.contains("exec")
            || n.contains("command")
            || t.contains("run")
            || t.contains("command")
        {
            Self::Command
        } else {
            Self::Other
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Read => ctext::STEP_READ.get(),
            Self::Edit => ctext::STEP_EDIT.get(),
            Self::Search => ctext::STEP_SEARCH.get(),
            Self::Command => ctext::STEP_COMMAND.get(),
            Self::Other => ctext::STEP_OTHER.get(),
        }
    }

    fn tone(self) -> ui::BadgeTone {
        match self {
            Self::Edit => ui::BadgeTone::Warning,
            Self::Read | Self::Search | Self::Command | Self::Other => ui::BadgeTone::Neutral,
        }
    }
}

pub(super) fn is_work_kind(kind: ChatItemKind) -> bool {
    matches!(
        kind,
        ChatItemKind::ToolCall | ChatItemKind::ToolResult | ChatItemKind::Reasoning
    )
}

pub(super) fn first_non_empty_line(text: &str) -> &str {
    text.lines()
        .find(|line| !line.trim().is_empty())
        .map_or("", str::trim)
}

fn call_detail(call: &ChatItem) -> &str {
    for candidate in [
        first_non_empty_line(&call.title),
        first_non_empty_line(&call.file_path),
        first_non_empty_line(&call.text),
        first_non_empty_line(&call.tool_name),
    ] {
        if !candidate.is_empty() {
            return candidate;
        }
    }
    ""
}

fn orphan_result_detail(result: &ChatItem) -> &str {
    let path = first_non_empty_line(&result.file_path);
    if path.is_empty() {
        first_non_empty_line(&result.text)
    } else {
        path
    }
}
