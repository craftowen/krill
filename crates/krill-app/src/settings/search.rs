//! 설정 검색(OWE-656). 항목별 제목(i18n 한·영 둘 다)·키워드 정적 색인으로 좌측 내비 탭을 거른다.
//! UI 언어와 상관없이 한글·영어 검색어가 모두 맞는다(Orca `settings-search.ts`와 같은 방식).

use super::AdminTab;
use super::text;
use crate::i18n::{Language, Message, SETTINGS_SEARCH_INDEX as INDEX};

/// 검색어와 맞는 탭([`AdminTab::ALL`] 순). 빈 검색어면 전부. 대소문자는 가리지 않는다.
/// 띄어쓰기로 나뉜 여러 단어는 같은 항목(탭 제목·부제·색인 한 줄) 안에
/// 모두 들어 있으면 맞는다("worktree cleanup" → `SPACE_TITLE` 영문 + 키워드).
pub(super) fn search_tabs(query: &str) -> Vec<AdminTab> {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return AdminTab::ALL.to_vec();
    }
    let words: Vec<&str> = query.split_whitespace().collect();
    let both = |m: Message| [Language::Ko, Language::En].map(|l| m.in_language(l));
    let doc_hit = |doc: &[String]| {
        doc.iter().any(|s| s.contains(&query))
            || words.iter().all(|w| doc.iter().any(|s| s.contains(w)))
    };
    let mut doc: Vec<String> = Vec::new();
    AdminTab::ALL
        .into_iter()
        .filter(|&tab| {
            doc.clear();
            doc.extend(both(tab_title(tab)).map(str::to_lowercase));
            doc.extend(both(tab.subtitle_message()).map(str::to_lowercase));
            if doc_hit(&doc) {
                return true;
            }
            INDEX
                .iter()
                .filter(|(t, ..)| *t == tab)
                .any(|(_, title, keywords)| {
                    doc.clear();
                    doc.extend(both(*title).map(str::to_lowercase));
                    doc.extend(keywords.iter().copied().map(str::to_lowercase));
                    doc_hit(&doc)
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
        for q in [
            "원격 Krill",
            "remote krill",
            "초대",
            "invite",
            "짝짓기",
            "p2p",
        ] {
            assert_eq!(search_tabs(q), [AdminTab::RemoteKrill], "{q}");
        }
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
