//! 설정 검색(OWE-656). 항목별 제목(i18n 한·영 둘 다)·키워드 정적 색인에서 본문 결과 목록을 만든다.
//! UI 언어와 상관없이 한글·영어 검색어가 모두 맞는다(Orca `settings-search.ts`와 같은 방식).

use super::AdminTab;
use crate::i18n::{Language, Message, SETTINGS_SEARCH_INDEX as INDEX};

/// 검색어와 맞는 설정. 색인 행, 그리고 색인 행이 안 맞은 페이지는 이름이나 부제가 맞을 때 페이지 자체.
/// 내비 탭 순서이며 부모 탭의 항목이 하위 페이지보다 앞이다. 빈 검색어면 비어 있다.
pub(super) fn search_hits(query: &str) -> Vec<(AdminTab, Message)> {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return Vec::new();
    }
    let hit = |s: &str| s.to_lowercase().contains(&query);
    let both = |m: Message| [Language::Ko, Language::En].map(|l| m.in_language(l));
    let mut hits: Vec<_> = INDEX
        .iter()
        .filter(|(_, title, words)| {
            both(*title).into_iter().any(hit) || words.iter().copied().any(hit)
        })
        .map(|(tab, title, _)| (*tab, *title))
        .collect();
    for tab in AdminTab::ALL.into_iter().chain(AdminTab::SUBPAGES) {
        if !hits.iter().any(|(t, _)| *t == tab)
            && (both(tab.title_message()).into_iter().any(hit)
                || both(tab.subtitle_message()).into_iter().any(hit))
        {
            hits.push((tab, tab.title_message()));
        }
    }
    hits.sort_by_key(|(tab, _)| (tab.page() as usize, *tab as usize));
    hits
}

/// 결과가 하나라도 있는 내비 탭. 빈 검색어면 전부.
#[cfg(test)]
pub(super) fn search_tabs(query: &str) -> Vec<AdminTab> {
    if query.trim().is_empty() {
        return AdminTab::ALL.to_vec();
    }
    let hits = search_hits(query);
    AdminTab::ALL
        .into_iter()
        .filter(|tab| hits.iter().any(|(hit, _)| hit.page() == *tab))
        .collect()
}

/// 일치하는 라벨에 네이티브 포커스와 중첩 스크롤 앵커를 연결한다.
#[derive(Clone)]
pub(super) struct SearchFocus {
    pub title: Option<Message>,
    pub scroll: gpui::ScrollHandle,
    pub anchor: gpui::ScrollAnchor,
    pub focus: gpui::FocusHandle,
    pub rendered: std::rc::Rc<std::cell::Cell<bool>>,
}

impl SearchFocus {
    pub fn new(cx: &gpui::App) -> Self {
        let scroll = gpui::ScrollHandle::default();
        Self {
            title: None,
            anchor: gpui::ScrollAnchor::for_handle(scroll.clone()),
            scroll,
            focus: cx.focus_handle(),
            rendered: std::rc::Rc::default(),
        }
    }

    pub fn label(
        &self,
        p: &crate::ui::Palette,
        title: &'static str,
        desc: impl crate::ui::primitives::LabelDesc,
    ) -> gpui::Stateful<gpui::Div> {
        use gpui::InteractiveElement as _;
        self.anchored(
            p,
            title,
            super::layout::label(p, title, desc)
                .id(gpui::SharedString::from(format!("settings-label-{title}"))),
        )
    }

    /// `title`이 검색 대상이면 `element`를 포커스와 스크롤 앵커로 삼는다.
    pub fn anchored(
        &self,
        p: &crate::ui::Palette,
        title: &'static str,
        element: gpui::Stateful<gpui::Div>,
    ) -> gpui::Stateful<gpui::Div> {
        use gpui::prelude::FluentBuilder as _;
        use gpui::{InteractiveElement as _, StatefulInteractiveElement as _, Styled as _};
        let matched = self.title.is_some_and(|target| target.get() == title);
        if matched {
            self.rendered.set(true);
        }
        element.when(matched, |d| {
            d.track_focus(&self.focus)
                .tab_index(0)
                .anchor_scroll(Some(self.anchor.clone()))
                .focus_visible(|d| d.bg(p.accent).rounded(crate::ui::RADIUS_SM))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn korean_and_english_queries_filter_tabs() {
        assert_eq!(search_tabs("글꼴"), [AdminTab::Appearance]);
        assert_eq!(search_tabs("font"), [AdminTab::Appearance]);
        assert_eq!(search_tabs("  FONT "), [AdminTab::Appearance]);
        // 파일 전용 옵션은 설정 파일 행으로 안내한다.
        for q in ["커서", "cursor", "옵션", "meta", "복사", "copy", "blink"] {
            assert!(search_tabs(q).contains(&AdminTab::System), "{q}");
        }
        assert_eq!(search_tabs("ssh"), [AdminTab::Machines]);
        for q in ["원격 Krill", "remote krill", "초대", "invite", "p2p"] {
            assert_eq!(search_tabs(q), [AdminTab::Machines], "{q}");
        }
        assert!(search_tabs("페어링").contains(&AdminTab::Machines));
        // 알림 이벤트(OWE-659)는 알림 항목에서 찾는다.
        for q in ["벨", "bell", "입력", "input", "승인", "완료", "done"] {
            assert!(search_tabs(q).contains(&AdminTab::System), "{q}");
        }
        // 탭 이름·부제(한·영 모두)도 맞는다(OWE-795).
        assert!(search_tabs("shortcuts").contains(&AdminTab::Shortcuts));
        assert_eq!(search_tabs("스마트폰"), [AdminTab::Mobile]);
        assert_eq!(search_tabs("qr"), [AdminTab::Mobile]);
        assert!(search_tabs("zzz-no-such-setting").is_empty());
    }

    #[test]
    fn search_reaches_segments_and_subpages() {
        let first_in = |query: &str, page: AdminTab| {
            search_hits(query)
                .into_iter()
                .find(|(tab, _)| tab.page() == page)
                .map(|(tab, _)| tab)
        };
        for (query, page, target) in [
            ("qr", AdminTab::Mobile, AdminTab::Mobile),
            ("초대", AdminTab::Machines, AdminTab::Machines),
            ("스마트폰", AdminTab::Mobile, AdminTab::Mobile),
            ("ssh", AdminTab::Machines, AdminTab::Machines),
            ("빠른 명령", AdminTab::Agents, AdminTab::QuickCommands),
            ("심링크", AdminTab::Agents, AdminTab::Project),
            ("디스크", AdminTab::System, AdminTab::Disk),
        ] {
            assert_eq!(first_in(query, page), Some(target), "{query}");
        }
    }

    #[test]
    fn hits_follow_nav_order_and_name_pages_without_index_rows() {
        let hits = search_hits("글꼴");
        assert_eq!(
            hits,
            [
                (AdminTab::Appearance, super::super::text::TERMINAL_FONT),
                (AdminTab::Appearance, super::super::text::FONT_SIZE),
            ]
        );
        // 부모 탭의 행이 먼저, 그다음 하위 페이지.
        let order: Vec<_> = search_hits("워크트리").into_iter().map(|h| h.0).collect();
        assert!(order.windows(2).all(|w| {
            (w[0].page() as usize, w[0] as usize) <= (w[1].page() as usize, w[1] as usize)
        }));
        // 색인 행이 안 맞은 페이지는 이름으로 나오되, 같은 페이지를 두 번 내지 않는다.
        assert_eq!(
            search_hits("shortcuts")
                .into_iter()
                .filter(|h| h.0 == AdminTab::Shortcuts)
                .count(),
            1
        );
        assert!(search_hits("빠른 명령").contains(&(
            AdminTab::QuickCommands,
            AdminTab::QuickCommands.title_message()
        )));
        assert!(search_hits("  ").is_empty());
    }

    #[test]
    fn empty_query_keeps_every_tab() {
        assert_eq!(search_tabs(""), AdminTab::ALL);
        assert_eq!(search_tabs("   "), AdminTab::ALL);
    }
}
