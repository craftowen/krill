//! 설정 창 공통 레이아웃 조각(OWE-651).
//!
//! 실구현은 디자인 시스템 공용 모듈([`crate::ui::primitives`])에 있으며,
//! 설정 탭과 대화상자·온보딩·사이드바가 모두 동일한 카드·행·라벨·컨트롤·키캡·위험 버튼 프리미티브를 공유한다.

pub(super) use crate::ui::primitives::{
    card, controls, danger_confirm as danger, keycap, label, row, secondary, section_title,
};
