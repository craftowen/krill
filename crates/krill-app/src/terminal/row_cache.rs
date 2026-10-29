//! 칸별 행 렌더 계획. 화면·테마가 그대로면 커서/선택/스피너 재그리기에서 재사용한다.

use std::cell::RefCell;
use std::rc::Rc;

use krill_proto::theme::Theme;
use krill_term::grid::{RowPlan, Screen, plan_row};

/// 보이는 터미널의 수명에 묶인 캐시. 폰트·배율·선택·커서는 행 계획에 들어가지 않는다.
/// 직전 화면 하나만 참조한다. 같은 위치를 먼저 보고, 한 줄 이동한 내용도 재사용한다.
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
        // 위/아래 한 줄 스크롤은 첫 두 miss 안에서 이웃을 찾는다(새로 들어온 첫 행 포함).
        // 전체 갱신이면 실패한 탐색을 두 번까지만 시도하고 이후 행은 바로 다시 만든다.
        let mut neighbor_misses = 0_usize;
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
                } else if same_theme
                    && neighbor_misses < 2
                    && let Some(previous) = cached.screen.as_ref()
                    // 한 줄 스크롤의 이웃만 확인한다. 같은 위치의 빠른 경로에는
                    // 후보 배열·탐색을 더하지 않고, miss당 추가 비교도 두 번으로 제한한다.
                    && let Some(plan) = [index.checked_add(1), index.checked_sub(1)]
                        .into_iter()
                        .flatten()
                        .find_map(|candidate| {
                            if previous.rows.get(candidate) == Some(row) {
                                cached.plans.get(candidate)
                            } else {
                                None
                            }
                        })
                {
                    Rc::clone(plan)
                } else {
                    neighbor_misses = neighbor_misses.saturating_add(1);
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
