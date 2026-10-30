//! 설정 검색(OWE-656). 항목별 제목(i18n 한·영 둘 다)·키워드 정적 색인으로 좌측 내비 탭을 거른다.
//! UI 언어와 상관없이 한글·영어 검색어가 모두 맞는다(Orca `settings-search.ts`와 같은 방식).

use super::AdminTab;
use super::text;
use crate::i18n::{Language, Message, SETTINGS_SEARCH_INDEX as INDEX};

/// 검색어와 맞는 탭([`AdminTab::ALL`] 순). 빈 검색어면 전부. 대소문자는 가리지 않는다.
pub(super) fn search_tabs(query: &str) -> Vec<AdminTab> {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return AdminTab::ALL.to_vec();
    }
    let hit = |s: &str| s.to_lowercase().contains(&query);
    let both = |m: Message| [Language::Ko, Language::En].map(|l| m.in_language(l));
    AdminTab::ALL
        .into_iter()
        .filter(|&tab| {
            both(tab_title(tab)).into_iter().any(hit)
                || both(tab.subtitle_message()).into_iter().any(hit)
                || INDEX
                    .iter()
                    .filter(|(t, ..)| *t == tab)
                    .any(|(_, title, words)| {
                        both(*title).into_iter().any(hit) || words.iter().copied().any(hit)
                    })
        })
        .collect()
}

fn tab_title(tab: AdminTab) -> Message {
    match tab {
        AdminTab::General => text::TAB_GENERAL,
        AdminTab::Terminal => text::TAB_TERMINAL,
        AdminTab::Accounts => text::TAB_ACCOUNTS,
        AdminTab::Agents => text::TAB_AGENTS,
        AdminTab::Install => text::TAB_INSTALL,
        AdminTab::Ssh => text::TAB_SSH,
        AdminTab::RemoteKrill => text::TAB_REMOTE_KRILL,
        AdminTab::Workflows => text::TAB_WORKFLOWS,
        AdminTab::Shortcuts => text::TAB_SHORTCUTS,
        AdminTab::Resources => text::TAB_RESOURCES,
        AdminTab::Mobile => crate::i18n::catalog::mobile::TAB,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn korean_and_english_queries_filter_tabs() {
        assert_eq!(search_tabs("글꼴"), [AdminTab::Terminal]);
        assert_eq!(search_tabs("font"), [AdminTab::Terminal]);
        assert_eq!(search_tabs("  FONT "), [AdminTab::Terminal]);
        // 터미널 탭(OWE-658) 키워드: 커서·Option·Meta·복사(한·영).
        for q in ["커서", "cursor", "옵션", "meta", "복사", "copy", "blink"] {
            assert!(search_tabs(q).contains(&AdminTab::Terminal), "{q}");
        }
        assert_eq!(search_tabs("ssh"), [AdminTab::Ssh]);
        for q in ["원격 Krill", "remote krill", "초대", "invite", "p2p"] {
            assert_eq!(search_tabs(q), [AdminTab::RemoteKrill], "{q}");
        }
        assert!(search_tabs("페어링").contains(&AdminTab::RemoteKrill));
        assert_eq!(search_tabs("동면"), [AdminTab::Agents]);
        assert!(search_tabs("webhook").contains(&AdminTab::Workflows));
        // 알림 이벤트별 토글·테스트 알림(OWE-659)은 일반 탭에서 찾는다.
        for q in [
            "벨",
            "bell",
            "입력",
            "input",
            "승인",
            "완료",
            "done",
            "테스트 알림",
            "test",
            "스타",
            "응원",
            "star",
            "github",
            "support",
        ] {
            assert!(search_tabs(q).contains(&AdminTab::General), "{q}");
        }
        // 탭 이름·부제(한·영 모두)도 맞는다(OWE-795).
        assert!(search_tabs("shortcuts").contains(&AdminTab::Shortcuts));
        assert_eq!(search_tabs("스마트폰"), [AdminTab::Mobile]);
        assert_eq!(search_tabs("qr"), [AdminTab::RemoteKrill, AdminTab::Mobile]);
        assert!(search_tabs("zzz-no-such-setting").is_empty());
    }

    #[test]
    fn empty_query_keeps_every_tab() {
        assert_eq!(search_tabs(""), AdminTab::ALL);
        assert_eq!(search_tabs("   "), AdminTab::ALL);
    }
}

/// 탭 제목보다 실제 설정 제목·키워드가 일치하는 행을 우선한다.
pub(super) fn first_setting(query: &str, tab: AdminTab) -> Option<Message> {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return None;
    }
    INDEX
        .iter()
        .filter(|(t, ..)| *t == tab)
        .find_map(|(_, title, words)| {
            ([Language::Ko, Language::En]
                .iter()
                .any(|lang| title.in_language(*lang).to_lowercase().contains(&query))
                || words
                    .iter()
                    .any(|word| word.to_lowercase().contains(&query)))
            .then_some(*title)
        })
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
        use gpui::prelude::FluentBuilder as _;
        use gpui::{InteractiveElement as _, StatefulInteractiveElement as _, Styled as _};
        let matched = self.title.is_some_and(|target| target.get() == title);
        if matched {
            self.rendered.set(true);
        }
        super::layout::label(p, title, desc)
            .id(gpui::SharedString::from(format!("settings-label-{title}")))
            .when(matched, |d| {
                d.track_focus(&self.focus)
                    .tab_index(0)
                    .anchor_scroll(Some(self.anchor.clone()))
                    .focus_visible(|d| d.bg(p.accent).rounded(crate::ui::RADIUS_SM))
            })
    }
}
