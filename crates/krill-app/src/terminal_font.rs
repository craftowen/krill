//! 터미널 고정폭 글꼴 해석(OWE-899).
//!
//! Orca 가져오기가 `terminalFontFamily = "SF Mono"`를 그대로 넣으면 터미널이 깨졌다.
//! `SF Mono`는 `/System/Library/Fonts/SFNSMono.ttf` 숨김 시스템 글꼴이라 gpui 글꼴 스택의
//! 패밀리 조회(`font-kit` `select_family_by_name`)에 걸리지 않고, `resolve_font`가 비례
//! 대체 글꼴로 조용히 대신한다. 그런데 셀 폭은 `'m'` advance로 재서 글자 사이가 벌어진다.
//! 그래서 그리는 쪽은 항상 "실제로 고정폭으로 해석되는" 패밀리만 쓴다.
//!
//! - [`is_usable_terminal_font`]: 해석된 글꼴의 advance가 글자마다 같은지(고정폭) 본다.
//!   이름 존재가 아니라 advance 균일성으로 판정하므로, 해석이 비례 폴백으로 떨어지면
//!   자동으로 쓸 수 없다고 나온다.
//! - [`effective_terminal_font`]: 쓸 수 없으면 Menlo로 폴백한다. 셀 폭(`terminal::cell_size`)과
//!   그리기(`TerminalElement::paint`)가 같은 이름을 쓰므로 셀과 글자가 어긋나지 않는다.
//! - 판정 로직([`uniform`])·대체 결정([`effective_font_with`])은 글꼴 시스템 없이 단위
//!   테스트한다. headless 테스트의 글꼴 목은 모든 advance를 균일하게 돌려줘서 실측이 안 된다.
//!
//! SF Mono 직접 지원(OWE-899 할 일 2)은 보류한다. gpui 스택은 패밀리 이름 조회만 해서
//! PostScript 이름(`SFMono-Regular`) 매칭이 안 되고, 메모리 등록(`add_fonts`)·벤더
//! 글꼴 로더 손질은 이 Mac에서 화면 검증이 금지돼 확인할 수 없다. 지정해도 Menlo로
//! 폴백되고 설정 화면에 사유가 뜬다.

use gpui::{SharedString, TextSystem, font, px};
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::{Arc, Weak};

/// 해석 실패·비고정폭 때 대신 쓰는 터미널 글꼴. 기본값(`ui::FONT_MONO`)과 같다.
pub const FALLBACK_FAMILY: &str = crate::ui::FONT_MONO;

/// 고정폭 판정에 쓸 글자들. 비례 글꼴이면 `i`·공백 등이 `m`·`W`보다 좁다.
const PROBE_CHARS: [char; 7] = ['i', 'm', 'W', '0', ' ', 'a', '-'];

/// 판정에 쓸 advance 개수.
pub const PROBE_LEN: usize = PROBE_CHARS.len();

/// 같은 글꼴 advance끼리 허용하는 오차(px). 고정폭은 폰트 단위가 정수라 정확히 같아야
/// 하지만 부동소수 변환을 감안해 둔다.
const PROBE_TOLERANCE_PX: f32 = 0.01;

/// 판정용 크기. advance는 크기에 비례하므로 값은 판정 결과에 영향이 없다.
const PROBE_SIZE: gpui::Pixels = px(12.);

/// advance 균일성(고정폭) 판정. 유한하지 않거나 차이가 허용치를 넘으면 거짓이다.
#[must_use]
pub fn uniform(widths: &[f32; PROBE_LEN]) -> bool {
    let mut iter = widths.iter();
    let Some(first) = iter.next() else {
        return false;
    };
    if !first.is_finite() {
        return false;
    }
    iter.all(|w| w.is_finite() && (w - first).abs() <= PROBE_TOLERANCE_PX)
}

/// 실제로 그릴 글꼴(OWE-899).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectiveFont {
    /// 그릴 때·셀 계산에 쓸 패밀리 이름.
    pub family: SharedString,
    /// 지정값이 못 쓰여 Menlo로 대신 그리는지.
    pub replaced: bool,
}

/// 글꼴 시스템에서 재는 어댑터. `None`이면 잴 수 없어 쓸 수 없는 것으로 친다.
fn measure(text: &TextSystem, family: &str) -> Option<[f32; PROBE_LEN]> {
    let id = text.resolve_font(&font(family));
    let mut out = [0.0; PROBE_LEN];
    for (slot, ch) in out.iter_mut().zip(PROBE_CHARS) {
        *slot = f32::from(text.advance(id, PROBE_SIZE, ch).ok()?.width);
    }
    Some(out)
}

/// 대체 결정의 순수 로직. `probe`는 패밀리별 advance(`None`이면 측정 실패)를 돌려준다.
/// 글꼴 시스템 없이 단위 테스트하기 위해 [`effective_terminal_font`]에서 분리했다.
#[must_use]
pub fn effective_font_with(
    requested: &str,
    mut probe: impl FnMut(&str) -> Option<[f32; PROBE_LEN]>,
) -> EffectiveFont {
    let trimmed = requested.trim();
    if !trimmed.is_empty() && probe(trimmed).is_some_and(|w| uniform(&w)) {
        return EffectiveFont {
            family: SharedString::from(trimmed.to_owned()),
            replaced: false,
        };
    }
    EffectiveFont {
        family: SharedString::new_static(FALLBACK_FAMILY),
        replaced: !trimmed.is_empty() && !trimmed.eq_ignore_ascii_case(FALLBACK_FAMILY),
    }
}

#[derive(Default)]
struct RegistryCache {
    registry: Weak<TextSystem>,
    entries: HashMap<String, (gpui::FontId, EffectiveFont, bool)>,
}
thread_local! {
    static CACHE: RefCell<RegistryCache> = RefCell::new(RegistryCache::default());
}

fn cached_font(text: &Arc<TextSystem>, requested: &str) -> (EffectiveFont, bool) {
    let requested = requested.trim();
    let resolved = text.resolve_font(&font(requested));
    CACHE.with_borrow_mut(|cache| {
        if !Weak::ptr_eq(&cache.registry, &Arc::downgrade(text)) {
            cache.registry = Arc::downgrade(text);
            cache.entries.clear();
        }
        // resolve_font is registry-generation aware (add_fonts clears its map).
        // Comparing the resolved id also invalidates negative/fallback decisions.
        if let Some((id, effective, usable)) = cache.entries.get(requested)
            && *id == resolved
        {
            return (effective.clone(), *usable);
        }
        let widths = (!requested.is_empty())
            .then(|| measure(text, requested))
            .flatten();
        let usable = widths.is_some_and(|w| uniform(&w));
        let effective = effective_font_with(requested, |_| widths);
        if cache.entries.len() >= 128 {
            cache.entries.clear();
        }
        cache
            .entries
            .insert(requested.to_owned(), (resolved, effective.clone(), usable));
        (effective, usable)
    })
}

/// 지정 글꼴을 쓰되, 고정폭으로 해석되지 않으면 Menlo로 폴백한다.
#[must_use]
pub fn effective_terminal_font(text: &Arc<TextSystem>, requested: &str) -> EffectiveFont {
    cached_font(text, requested).0
}

/// 해석된 글꼴이 터미널 셀 계산에 쓸 수 있는 고정폭인지 본다(OWE-899).
///
/// 빈 이름은 무조건 거짓이다. 없는 패밀리는 gpui가 (보통 비례) 글꼴로 대신하는데
/// 그 경우 advance가 글자마다 달라 거짓이 된다.
#[must_use]
pub fn is_usable_terminal_font(text: &Arc<TextSystem>, family: &str) -> bool {
    cached_font(text, family).1
}

/// 드롭다운에 올릴 고정폭 글꼴만 고른다(OWE-899). 순서는 호출자가 정한다.
#[must_use]
pub fn usable_terminal_fonts(text: &Arc<TextSystem>, names: &[String]) -> Vec<String> {
    names
        .iter()
        .filter(|n| is_usable_terminal_font(text, n))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const MONO: [f32; PROBE_LEN] = [7.2; PROBE_LEN];
    const PROPORTIONAL: [f32; PROBE_LEN] = [4.1, 8.3, 10.2, 7.2, 3.5, 7.0, 4.4];

    #[gpui::test]
    fn cached_effective_font_is_scoped_to_registry_and_resolved_id(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            let text = cx.text_system();
            let expected = effective_font_with("Menlo", |family| measure(text, family));
            assert_eq!(effective_terminal_font(text, " Menlo "), expected);
            CACHE.with_borrow_mut(|cache| {
                let entry = cache.entries.get_mut("Menlo").unwrap();
                entry.0 = gpui::FontId(usize::MAX);
                entry.1.family = "stale".into();
            });
            assert_eq!(effective_terminal_font(text, "Menlo"), expected);
            CACHE.with_borrow_mut(|cache| {
                cache.registry = Weak::new();
                cache.entries.get_mut("Menlo").unwrap().1.family = "other-registry".into();
            });
            assert_eq!(effective_terminal_font(text, "Menlo"), expected);
            assert_eq!(
                effective_terminal_font(text, ""),
                effective_font_with("", |_| None)
            );
        });
    }

    #[test]
    fn uniform_accepts_only_equal_finite_advances() {
        assert!(uniform(&MONO));
        assert!(!uniform(&PROPORTIONAL));
        assert!(!uniform(&[7.2, 7.2, 7.2, 7.2, 7.2, 7.2, f32::NAN]));
        assert!(!uniform(&[7.2, 7.2, 7.2, 7.2, 7.2, 7.2, 7.25]));
    }

    #[test]
    fn usable_request_is_kept() {
        let eff = effective_font_with("Menlo", |_| Some(MONO));
        assert_eq!(eff.family.as_ref(), "Menlo");
        assert!(!eff.replaced);
        // 앞뒤 공백은 잘라 쓴다.
        let eff = effective_font_with("  Menlo  ", |name| (name == "Menlo").then_some(MONO));
        assert_eq!(eff.family.as_ref(), "Menlo");
        assert!(!eff.replaced);
    }

    #[test]
    fn missing_request_falls_back_to_menlo() {
        let eff = effective_font_with("이런글꼴은없다-owe899", |_| None);
        assert_eq!(eff.family.as_ref(), FALLBACK_FAMILY);
        assert!(eff.replaced);
    }

    #[test]
    fn proportional_request_falls_back_to_menlo() {
        // OWE-899 재현: SF Mono처럼 해석이 비례 폴백으로 떨어진 경우.
        let eff = effective_font_with("SF Mono", |_| Some(PROPORTIONAL));
        assert_eq!(eff.family.as_ref(), FALLBACK_FAMILY);
        assert!(eff.replaced);
    }

    #[test]
    fn blank_request_is_default_without_warning() {
        for requested in ["", "   "] {
            let eff = effective_font_with(requested, |_| Some(MONO));
            assert_eq!(eff.family.as_ref(), FALLBACK_FAMILY);
            assert!(!eff.replaced);
        }
    }

    #[test]
    fn fallback_name_itself_never_warns() {
        let eff = effective_font_with(FALLBACK_FAMILY, |_| None);
        assert_eq!(eff.family.as_ref(), FALLBACK_FAMILY);
        assert!(!eff.replaced);
    }
}
