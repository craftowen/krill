use super::*;

fn rich_screen() -> Screen {
    let mut screen = Screen::blank(12, 5);
    for (index, row) in screen.rows.iter_mut().enumerate() {
        row.resize(12, Cell::default());
        row[0].ch = char::from(b'a' + u8::try_from(index).unwrap());
        row[0].flags = Flags::LINK;
        row[1].ch = '한';
        row[1].flags = Flags::WIDE | Flags::BOLD;
        row[2].flags = Flags::SPACER;
        row[3].ch = 'ᄀ';
        row[3].zerowidth = vec!['ᅡ'];
        row[4].ch = '👩';
        row[4].zerowidth = vec!['\u{200d}', '💻'];
        row[4].flags = Flags::WIDE;
        row[5].flags = Flags::SPACER;
        row[6].ch = '❤';
        row[6].zerowidth = vec!['\u{fe0f}'];
        row[8].ch = 'x';
        row[8].fg = Color::Indexed(3);
        row[8].bg = Color::Indexed(4);
        row[8].underline = Color::Indexed(5);
        row[8].flags = Flags::UNDERLINE | Flags::STRIKEOUT | Flags::DIM;
        row[9].ch = 'h';
        row[9].flags = Flags::HIDDEN;
    }
    screen
}

fn assert_direct(screen: &Screen, theme: &Theme, plans: &[Rc<RowPlan>]) {
    assert_eq!(plans.len(), screen.rows.len());
    for (row, plan) in screen.rows.iter().zip(plans.iter()) {
        assert_eq!(plan.as_ref(), &plan_row(row, theme));
    }
}

#[test]
fn reuses_shifted_cjk_and_styled_rows_in_both_directions() {
    for upward in [true, false] {
        let cache = RowPlanCache::default();
        let old = Rc::new(rich_screen());
        let previous = cache.plans(&old, &DEFAULT);
        let mut next = old.as_ref().clone();
        if upward {
            next.rows.rotate_left(1);
            next.rows.last_mut().unwrap()[0].ch = 'z';
        } else {
            next.rows.rotate_right(1);
            next.rows[0][0].ch = 'z';
        }
        let next = Rc::new(next);
        let plans = cache.plans(&next, &DEFAULT);
        assert_direct(&next, &DEFAULT, &plans);
        for index in 0..plans.len() - 1 {
            let (new_index, old_index) = if upward {
                (index, index + 1)
            } else {
                (index + 1, index)
            };
            assert!(
                Rc::ptr_eq(&plans[new_index], &previous[old_index]),
                "unchanged rows reuse their plans after shifting: upward={upward}, row={new_index}"
            );
        }
    }
}

#[test]
fn shifted_rows_compare_every_cell_property_and_invalidate_theme() {
    for property in 0..7 {
        let cache = RowPlanCache::default();
        let old = Rc::new(rich_screen());
        let previous = cache.plans(&old, &DEFAULT);
        let mut next = old.as_ref().clone();
        next.rows.rotate_left(1);
        match property {
            0 => next.rows[0][8].ch = 'y',
            1 => next.rows[0][8].zerowidth.push('\u{301}'),
            2 => next.rows[0][8].fg = Color::Indexed(6),
            3 => next.rows[0][8].bg = Color::Indexed(6),
            4 => next.rows[0][8].underline = Color::Indexed(6),
            5 => next.rows[0][8].flags |= Flags::ITALIC,
            6 => {
                next.rows[0].pop();
            }
            _ => unreachable!(),
        }
        let next = Rc::new(next);
        let plans = cache.plans(&next, &DEFAULT);
        assert_direct(&next, &DEFAULT, &plans);
        assert!(!previous.iter().any(|old| Rc::ptr_eq(old, &plans[0])));
        assert!(Rc::ptr_eq(&previous[2], &plans[1]));

        let mut light = DEFAULT;
        light.background = Rgb {
            r: 250,
            g: 250,
            b: 250,
        };
        let themed = cache.plans(&old, &light);
        assert_direct(&old, &light, &themed);
        for plan in themed.iter() {
            assert!(!plans.iter().any(|old| Rc::ptr_eq(old, plan)));
        }
    }
}

#[test]
fn duplicate_rows_and_link_targets_use_the_current_screen() {
    let cache = RowPlanCache::default();
    let mut old = rich_screen();
    old.rows[2] = old.rows[1].clone();
    let old = Rc::new(old);
    let previous = cache.plans(&old, &DEFAULT);
    let weak = Rc::downgrade(&old);
    let mut next = old.as_ref().clone();
    next.rows.rotate_left(1);
    next.rows[4].clear();
    next.links = vec![vec![krill_proto::screen::Link {
        column: 0,
        cols: 1,
        uri: "https://example.com/current".into(),
    }]];
    let next = Rc::new(next);
    let plans = cache.plans(&next, &DEFAULT);
    assert_direct(&next, &DEFAULT, &plans);
    assert!(Rc::ptr_eq(&previous[1], &plans[0]));
    assert!(Rc::ptr_eq(&previous[1], &plans[1]));
    assert_eq!(cache.0.borrow().screen.as_ref().unwrap().links, next.links);
    drop(old);
    assert!(weak.upgrade().is_none());
}

#[test]
fn failed_neighbor_searches_do_not_disable_reuse_on_later_screens() {
    let cache = RowPlanCache::default();
    let first = Rc::new(rich_screen());
    cache.plans(&first, &DEFAULT);
    let mut replaced = first.as_ref().clone();
    for (index, row) in replaced.rows.iter_mut().enumerate() {
        row[0].ch = char::from(b'p' + u8::try_from(index).unwrap());
    }
    let replaced = Rc::new(replaced);
    let previous = cache.plans(&replaced, &DEFAULT);
    assert_direct(&replaced, &DEFAULT, &previous);

    let mut scrolled = replaced.as_ref().clone();
    scrolled.rows.rotate_left(1);
    scrolled.rows.last_mut().unwrap()[0].ch = 'z';
    let scrolled = Rc::new(scrolled);
    let plans = cache.plans(&scrolled, &DEFAULT);
    assert_direct(&scrolled, &DEFAULT, &plans);
    for index in 0..plans.len() - 1 {
        assert!(Rc::ptr_eq(&previous[index + 1], &plans[index]));
    }
}

proptest::proptest! {
    #[test]
    fn row_reuse_matches_direct_plans_after_mixed_updates(
        operations in proptest::collection::vec((0_u8..10, proptest::prelude::any::<u16>(), proptest::prelude::any::<char>()), 0..48)
    ) {
        let cache = RowPlanCache::default();
        let mut current = Rc::new(rich_screen());
        let mut theme = DEFAULT;
        cache.plans(&current, &theme);
        for (operation, at, ch) in operations {
            let mut next = current.as_ref().clone();
            match operation {
                0 if !next.rows.is_empty() => next.rows.rotate_left(1),
                1 if !next.rows.is_empty() => next.rows.rotate_right(1),
                2..=6 => {
                    if let Some(row) = next.rows.get_mut(usize::from(at) % usize::from(next.lines.max(1))) {
                        let length = row.len().max(1);
                        if let Some(cell) = row.get_mut(usize::from(at) % length) {
                            match operation {
                                2 => cell.ch = ch,
                                3 => cell.zerowidth = vec![ch],
                                4 => cell.fg = Color::Indexed(u8::try_from(at % 256).unwrap()),
                                5 => cell.bg = Color::Indexed(u8::try_from(at % 256).unwrap()),
                                _ => cell.flags |= Flags::UNDERLINE,
                            }
                        }
                    }
                }
                7 => theme.foreground.r = u8::try_from(at % 256).unwrap(),
                8 => {
                    next.cols = at % 17;
                    for row in &mut next.rows {
                        row.resize(usize::from(next.cols), Cell::default());
                    }
                }
                9 => next = Screen::blank(at % 17, at % 7),
                _ => {}
            }
            current = Rc::new(next);
            let plans = cache.plans(&current, &theme);
            assert_direct(&current, &theme, &plans);
        }
    }
}
