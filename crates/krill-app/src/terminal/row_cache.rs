//! 칸별 행 렌더 계획. 화면·테마가 그대로면 커서/선택/스피너 재그리기에서 재사용한다.

use std::cell::RefCell;
use std::rc::Rc;

use krill_proto::theme::Theme;
use krill_term::grid::{RowPlan, Screen, plan_row};

/// 보이는 터미널의 수명에 묶인 캐시. 폰트·배율·선택·커서는 행 계획에 들어가지 않는다.
/// 직전 화면 하나만 참조하며 새 화면에서는 내용이 같은 행의 계획만 재사용한다.
#[derive(Debug, Clone, Default)]
pub struct RowPlanCache(Rc<RefCell<Rows>>);

#[derive(Debug, Default)]
struct Rows {
    screen: Option<Rc<Screen>>,
    theme: Option<Theme>,
    plans: Rc<[Rc<RowPlan>]>,
}

impl RowPlanCache {
    pub(super) fn plans(&self, screen: &Rc<Screen>, theme: &Theme) -> Rc<[Rc<RowPlan>]> {
        let mut cached = self.0.borrow_mut();
        let same_theme = cached.theme.as_ref() == Some(theme);
        if same_theme
            && cached
                .screen
                .as_ref()
                .is_some_and(|previous| Rc::ptr_eq(previous, screen))
        {
            return Rc::clone(&cached.plans);
        }
        let plans = screen
            .rows
            .iter()
            .enumerate()
            .map(|(index, row)| {
                if same_theme
                    && cached
                        .screen
                        .as_ref()
                        .and_then(|previous| previous.rows.get(index))
                        == Some(row)
                    && let Some(plan) = cached.plans.get(index)
                {
                    Rc::clone(plan)
                } else {
                    Rc::new(plan_row(row, theme))
                }
            })
            .collect();
        cached.plans = plans;
        cached.screen = Some(Rc::clone(screen));
        cached.theme = Some(*theme);
        Rc::clone(&cached.plans)
    }
}

#[cfg(test)]
mod tests;
