use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::hash::{Hash as _, Hasher as _};
use std::ops::Range;
use std::sync::Arc;

use gpui::{
    App, BorderStyle, Bounds, Corners, Edges, Element, ElementId, GlobalElementId, HighlightStyle,
    Hitbox, HitboxBehavior, Hsla, InspectorElementId, IntoElement, LayoutId, PaintQuad, Pixels,
    Point, SharedString, StyledText, TextLayout, Window, transparent_black,
};
use gpui_component::highlighter::{HighlightTheme, SyntaxHighlighter};
use gpui_component::{
    ActiveTheme as _, Rope, TextSelection, TextSelectionHandle, TextSelectionRegistration,
    TextSelectionRun,
};

/// Fenced code block text with syntax highlights that participates in window-scoped `TextSelection`.
pub(super) struct SelectableHighlightedText {
    id: ElementId,
    text: SharedString,
    styled_text: StyledText,
    selection_color: Hsla,
}

impl SelectableHighlightedText {
    pub(super) fn new(
        id: impl Into<ElementId>,
        language_tag: &str,
        code: impl Into<SharedString>,
        cx: &App,
    ) -> Self {
        let text: SharedString = code.into();
        let theme = cx.theme();
        let runs = highlight_code_runs_cached(language_tag, text.as_ref(), &theme.highlight_theme);
        let styled_text = if runs.is_empty() {
            StyledText::new(text.clone())
        } else {
            StyledText::new(text.clone()).with_highlights(runs.iter().cloned())
        };
        Self {
            id: id.into(),
            text,
            styled_text,
            selection_color: theme.selection,
        }
    }

    fn paint_selection(layout: &TextLayout, range: Range<usize>, color: Hsla, window: &mut Window) {
        let (Some(start), Some(end)) = (
            layout.position_for_index(range.start),
            layout.position_for_index(range.end),
        ) else {
            return;
        };
        for bounds in selection_quad_bounds(start, end, layout.bounds(), layout.line_height()) {
            window.paint_quad(PaintQuad {
                bounds,
                background: color.into(),
                corner_radii: Corners::default(),
                border_widths: Edges::default(),
                border_color: transparent_black(),
                border_style: BorderStyle::default(),
            });
        }
    }
}

fn selection_quad_bounds(
    start: Point<Pixels>,
    end: Point<Pixels>,
    bounds: Bounds<Pixels>,
    line_height: Pixels,
) -> Vec<Bounds<Pixels>> {
    if start.y == end.y {
        return vec![Bounds::from_corners(
            start,
            Point::new(end.x, end.y + line_height),
        )];
    }

    let mut quads = vec![Bounds::from_corners(
        start,
        Point::new(bounds.right(), start.y + line_height),
    )];
    if end.y > start.y + line_height {
        quads.push(Bounds::from_corners(
            Point::new(bounds.left(), start.y + line_height),
            Point::new(bounds.right(), end.y),
        ));
    }
    quads.push(Bounds::from_corners(
        Point::new(bounds.left(), end.y),
        Point::new(end.x, end.y + line_height),
    ));
    quads
}

impl IntoElement for SelectableHighlightedText {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for SelectableHighlightedText {
    type RequestLayoutState = TextSelectionHandle;
    type PrepaintState = Hitbox;

    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        global_id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let handle = if let Some(global_id) = global_id {
            window.with_element_state(global_id, |retained: Option<TextSelectionHandle>, _| {
                let handle =
                    retained.unwrap_or_else(|| TextSelectionHandle::new(self.text.clone(), cx));
                (handle.clone(), handle)
            })
        } else {
            TextSelectionHandle::new(self.text.clone(), cx)
        };
        let (layout_id, ()) = self
            .styled_text
            .request_layout(global_id, inspector_id, window, cx);
        (layout_id, handle)
    }

    fn prepaint(
        &mut self,
        global_id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        handle: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        self.styled_text
            .prepaint(global_id, inspector_id, bounds, &mut (), window, cx);
        let hitbox = window.insert_hitbox(bounds, HitboxBehavior::Normal);
        let registration = TextSelectionRegistration::new(hitbox.clone(), bounds)
            .with_text_bounds(vec![bounds])
            .with_rendered_element(handle, window, cx);
        handle.register(registration, window, cx);
        hitbox
    }

    fn paint(
        &mut self,
        global_id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        handle: &mut Self::RequestLayoutState,
        _: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let layout = self.styled_text.layout().clone();
        let selected_text_before = TextSelection::selected_text(window, cx);
        let projection = handle.update_runs(
            &[TextSelectionRun::new(
                self.text.clone(),
                layout.clone(),
                bounds,
            )],
            cx,
        );
        if selected_text_before != TextSelection::selected_text(window, cx) {
            window.refresh();
        }
        for range in projection.ranges().iter().flatten().cloned() {
            Self::paint_selection(&layout, range, self.selection_color, window);
        }
        self.styled_text.paint(
            global_id,
            inspector_id,
            bounds,
            &mut (),
            &mut (),
            window,
            cx,
        );
    }
}

pub(super) fn normalize_fence_language_key(tag: &str) -> String {
    let token = tag
        .split(|c: char| c.is_whitespace() || c == ',' || c == ';')
        .next()
        .unwrap_or("")
        .trim_matches(|c: char| matches!(c, '{' | '}' | '`' | '"' | '\''))
        .trim()
        .to_ascii_lowercase();
    match token.as_str() {
        "rs" | "rust" => "rust".to_owned(),
        "json" | "jsonc" | "json5" => "json".to_owned(),
        _ => token,
    }
}

const MAX_HIGHLIGHT_CACHE_ENTRIES: usize = 256;
const MAX_HIGHLIGHT_CACHE_CODE_BYTES: usize = 64 * 1024;

type HighlightRuns = Arc<[(Range<usize>, HighlightStyle)]>;

#[derive(Clone, PartialEq, Eq, Hash)]
struct HighlightCacheKey {
    lang: String,
    theme_sig: u64,
    code_hash: u64,
    code_len: usize,
}

struct HighlightCacheEntry {
    code: String,
    runs: HighlightRuns,
}

#[derive(Default)]
struct HighlightCache {
    map: HashMap<HighlightCacheKey, HighlightCacheEntry>,
    order: VecDeque<HighlightCacheKey>,
}

fn highlight_theme_signature(theme: &HighlightTheme) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    theme.name.hash(&mut hasher);
    theme.appearance.hash(&mut hasher);
    if let Some(bg) = theme.style.editor_background {
        bg.h.to_bits().hash(&mut hasher);
        bg.s.to_bits().hash(&mut hasher);
        bg.l.to_bits().hash(&mut hasher);
        bg.a.to_bits().hash(&mut hasher);
    }
    if let Some(fg) = theme.style.editor_foreground {
        fg.h.to_bits().hash(&mut hasher);
        fg.s.to_bits().hash(&mut hasher);
        fg.l.to_bits().hash(&mut hasher);
        fg.a.to_bits().hash(&mut hasher);
    }
    hasher.finish()
}

pub(super) fn highlight_code_runs_cached(
    language_tag: &str,
    code: &str,
    highlight_theme: &HighlightTheme,
) -> HighlightRuns {
    if code.is_empty() {
        return Arc::from([]);
    }
    let key_lang = normalize_fence_language_key(language_tag);
    if key_lang.is_empty() {
        return Arc::from([]);
    }
    if code.len() > MAX_HIGHLIGHT_CACHE_CODE_BYTES {
        return Arc::from(compute_highlight_runs(&key_lang, code, highlight_theme));
    }

    let mut code_hasher = std::collections::hash_map::DefaultHasher::new();
    code.hash(&mut code_hasher);
    let cache_key = HighlightCacheKey {
        lang: key_lang.clone(),
        theme_sig: highlight_theme_signature(highlight_theme),
        code_hash: code_hasher.finish(),
        code_len: code.len(),
    };

    thread_local! {
        static RUNS_CACHE: RefCell<HighlightCache> = RefCell::new(HighlightCache::default());
    }

    if let Some(hit) = RUNS_CACHE.with(|cell| {
        cell.borrow()
            .map
            .get(&cache_key)
            .filter(|entry| entry.code == code)
            .map(|entry| Arc::clone(&entry.runs))
    }) {
        return hit;
    }

    let runs: HighlightRuns = Arc::from(compute_highlight_runs(&key_lang, code, highlight_theme));
    RUNS_CACHE.with(|cell| {
        let mut cache = cell.borrow_mut();
        if !cache.map.contains_key(&cache_key) {
            while cache.map.len() >= MAX_HIGHLIGHT_CACHE_ENTRIES {
                let Some(oldest) = cache.order.pop_front() else {
                    break;
                };
                cache.map.remove(&oldest);
            }
            cache.order.push_back(cache_key.clone());
        }
        cache.map.insert(
            cache_key,
            HighlightCacheEntry {
                code: code.to_owned(),
                runs: Arc::clone(&runs),
            },
        );
    });
    runs
}

#[cfg(test)]
pub(super) fn highlight_code_runs(
    language_tag: &str,
    code: &str,
    highlight_theme: &HighlightTheme,
) -> Vec<(Range<usize>, HighlightStyle)> {
    highlight_code_runs_cached(language_tag, code, highlight_theme).to_vec()
}

fn compute_highlight_runs(
    key: &str,
    code: &str,
    highlight_theme: &HighlightTheme,
) -> Vec<(Range<usize>, HighlightStyle)> {
    let raw = if matches!(key, "rust" | "json") {
        highlight_with_treesitter(key, code, highlight_theme)
    } else {
        krill_app_core::editor::syntax::tokenize_for(key, code)
            .into_iter()
            .filter_map(|(range, kind)| highlight_theme.style(kind).map(|style| (range, style)))
            .collect()
    };

    sanitize_highlight_runs(code, raw)
}

fn highlight_with_treesitter(
    lang: &str,
    code: &str,
    highlight_theme: &HighlightTheme,
) -> Vec<(Range<usize>, HighlightStyle)> {
    thread_local! {
        static HIGHLIGHTERS: RefCell<HashMap<String, SyntaxHighlighter>> =
            RefCell::new(HashMap::new());
    }
    HIGHLIGHTERS.with(|cache| {
        let mut cache = cache.borrow_mut();
        let highlighter = cache
            .entry(lang.to_owned())
            .or_insert_with(|| SyntaxHighlighter::new(lang));
        let rope = Rope::from_str(code);
        highlighter.update(None, &rope, None);
        highlighter.styles(&(0..code.len()), highlight_theme)
    })
}

fn sanitize_highlight_runs(
    code: &str,
    mut runs: Vec<(Range<usize>, HighlightStyle)>,
) -> Vec<(Range<usize>, HighlightStyle)> {
    let len = code.len();
    runs.retain(|(r, _)| {
        r.start < r.end
            && r.end <= len
            && code.is_char_boundary(r.start)
            && code.is_char_boundary(r.end)
    });
    runs.sort_by_key(|(r, _)| (r.start, r.end));
    let mut out = Vec::with_capacity(runs.len());
    let mut cursor = 0usize;
    for (r, style) in runs {
        let start = r.start.max(cursor);
        if start < r.end {
            out.push((start..r.end, style));
            cursor = r.end;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn highlight_code_runs_covers_rust_json_and_python_safely() {
        let theme = HighlightTheme::default_dark();

        let rust_runs = highlight_code_runs("rs", "fn main() { let x = 1; }", &theme);
        assert!(!rust_runs.is_empty(), "rust fence should highlight");

        let json_runs = highlight_code_runs("json", "{\"ok\": true, \"n\": 42}", &theme);
        assert!(!json_runs.is_empty(), "json fence should highlight");

        let py_runs = highlight_code_runs("python", "def f():\n    return '한글'\n", &theme);
        assert!(!py_runs.is_empty(), "python fence should highlight");
        for (r, _) in &py_runs {
            assert!(r.start < r.end && r.end <= "def f():\n    return '한글'\n".len());
        }
    }
}
