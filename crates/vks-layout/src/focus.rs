//! Фокусные раскладки: [`FocusLayout`] — геометрия «крупный тайл + полоса
//! мелких», и правила Codian §2.2 о том, кто попадает в главное окно.
//!
//! Инварианты геометрии: фокус получает > 50% площади; остальные — мелкие;
//! сумма площадей == canvas (без пробелов и пересечений).

use crate::core::{Layout, ParticipantId, SlotId};
use crate::rect::{Canvas, Rect};

/// Генератор геометрии фокусных раскладок (focus + thumbnails).
pub struct FocusLayout;

impl FocusLayout {
    /// Геометрия фокусного семейства по номеру слота: слот 0 — крупный
    /// тайл (верхние 75% canvas), остальные — полоса снизу (25%).
    ///
    /// Это единственная реализация фокусной геометрии в дереве: `McuEngine`
    /// строит раскладки `focus`/`speaker` через неё, а не своей копией
    /// (B8: мёртвый `FocusLayout::compute` и инлайн-дубль в движке свёрнуты
    /// в одну функцию).
    #[must_use]
    pub fn slot_rects(canvas: &Canvas, slots: u32) -> Vec<Rect> {
        if canvas.is_empty() || slots == 0 {
            return Vec::new();
        }
        if slots == 1 {
            return vec![canvas.as_rect()];
        }
        let large_h = canvas.height * 75 / 100;
        let strip_h = canvas.height - large_h;
        let m = slots - 1;

        let mut rects = Vec::with_capacity(slots as usize);
        // Слот 0: крупный тайл на всю ширину сверху.
        rects.push(Rect::new(0, 0, canvas.width, large_h));
        // Слоты 1..: полоса миниатюр снизу, поровну.
        for k in 0..m {
            rects.push(Rect::new(canvas.width * k / m, large_h, canvas.width / m, strip_h));
        }
        rects
    }
}

/// Крупнейшее окно раскладки — «главное окно» в терминах Codian (§2.2).
///
/// Определяется по площади, а не по номеру слота: функция работает и для
/// фокусных семейств (крупный тайл сверху), и для фиксированных макетов PNM,
/// где номер крупного слота произволен. При равной площади побеждает меньший
/// `id`, чтобы результат не зависел от порядка слотов в векторе.
#[must_use]
pub fn main_window(layout: &Layout) -> Option<SlotId> {
    layout.slots.iter().max_by_key(|slot| (slot.rect.area(), std::cmp::Reverse(slot.id.as_u32()))).map(|slot| slot.id)
}

/// Переносит участника в главное окно раскладки (Codian §2.2).
///
/// Применяется к «Важному» участнику (`Important`) и к активному спикеру: они
/// обязаны быть в самом крупном окне независимо от того, кто говорит громче.
/// Жильцы главного окна и окна участника **меняются местами** — иначе прежний
/// владелец крупного окна пропал бы из раскладки.
///
/// Возвращает `true`, если раскладка изменилась. Участник, которого нет в
/// раскладке (не публикует видео), ничего не меняет.
pub fn promote_to_main_window(layout: &mut Layout, participant: ParticipantId) -> bool {
    let Some(main) = main_window(layout) else {
        return false;
    };
    let Some(current) = layout.slots.iter().find(|slot| slot.participant_id == Some(participant)).map(|slot| slot.id)
    else {
        return false;
    };
    if current == main {
        return false;
    }
    let displaced = layout.slots.iter().find(|slot| slot.id == main).and_then(|slot| slot.participant_id);
    for slot in &mut layout.slots {
        if slot.id == main {
            slot.participant_id = Some(participant);
        } else if slot.id == current {
            slot.participant_id = displaced;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::LayoutKind;

    fn pid(i: u128) -> ParticipantId {
        ParticipantId::new(i)
    }

    fn total_area(rects: &[Rect]) -> u64 {
        rects.iter().map(|r| r.area()).sum()
    }

    fn assert_no_overlap(rects: &[Rect]) {
        for i in 0..rects.len() {
            for j in (i + 1)..rects.len() {
                assert!(!rects[i].intersects(&rects[j]), "rects {i} and {j} overlap");
            }
        }
    }

    fn assert_inside(canvas: &Canvas, rects: &[Rect]) {
        for r in rects {
            assert!(r.x + r.w <= canvas.width && r.y + r.h <= canvas.height);
        }
    }

    #[test]
    fn single_slot_fills_canvas() {
        let canvas = Canvas::new(1920, 1080);
        let rects = FocusLayout::slot_rects(&canvas, 1);
        assert_eq!(rects.len(), 1);
        assert_eq!(rects[0], canvas.as_rect());
    }

    #[test]
    fn focus_gets_majority_and_tiles_cover_canvas() {
        let canvas = Canvas::new(1920, 1080);
        let rects = FocusLayout::slot_rects(&canvas, 4);
        assert_eq!(rects.len(), 4);
        assert_eq!(total_area(&rects), canvas.area());
        assert_no_overlap(&rects);
        assert_inside(&canvas, &rects);

        let focus_area = rects[0].area();
        assert!(focus_area > canvas.area() / 2, "фокус обязан занимать > 50%");
        for (i, r) in rects.iter().enumerate().skip(1) {
            assert!(r.area() < focus_area, "не-фокус {i} не меньше фокуса");
        }
    }

    #[test]
    fn large_tile_is_top_full_width_and_strip_follows() {
        let canvas = Canvas::new(1920, 1080);
        let rects = FocusLayout::slot_rects(&canvas, 3);
        let large = rects[0];
        assert_eq!(large.x, 0);
        assert_eq!(large.y, 0);
        assert_eq!(large.w, canvas.width);
        assert_eq!(large.h, canvas.height * 75 / 100);
        let strip_top = canvas.height * 75 / 100;
        assert_eq!(rects[1].y, strip_top);
        assert_eq!(rects[2].y, strip_top);
        assert_eq!(rects[1].x, 0);
        assert_eq!(rects[2].x, canvas.width / 2);
    }

    #[test]
    fn empty_canvas_or_zero_slots_give_no_rects() {
        let canvas = Canvas::new(0, 0);
        assert!(FocusLayout::slot_rects(&canvas, 4).is_empty());
        let canvas = Canvas::new(100, 100);
        assert!(FocusLayout::slot_rects(&canvas, 0).is_empty());
    }

    fn two_by_two() -> Layout {
        let mut layout = Layout::new(LayoutKind::Grid);
        layout.slots.push(crate::core::Slot::new(0, Some(pid(1)), Rect::new(0, 0, 960, 540)));
        layout.slots.push(crate::core::Slot::new(1, Some(pid(2)), Rect::new(960, 0, 960, 540)));
        layout.slots.push(crate::core::Slot::new(2, Some(pid(3)), Rect::new(0, 540, 960, 540)));
        layout.slots.push(crate::core::Slot::new(3, Some(pid(4)), Rect::new(960, 540, 960, 540)));
        layout
    }

    #[test]
    fn main_window_is_the_largest_by_area_not_by_id() {
        let mut layout = two_by_two();
        // Слот 2 — крупный тайл: главное окно определяется площадью.
        layout.slots[2].rect = Rect::new(0, 0, 1920, 810);
        assert_eq!(main_window(&layout), Some(SlotId::new(2)));
        assert_eq!(main_window(&Layout::new(LayoutKind::Grid)), None, "пустая раскладка без окон");
    }

    #[test]
    fn promotion_swaps_occupants_and_keeps_everyone_visible() {
        let mut layout = two_by_two();
        layout.slots[1].rect = Rect::new(0, 0, 1920, 810);
        assert_eq!(main_window(&layout), Some(SlotId::new(1)));

        assert!(promote_to_main_window(&mut layout, pid(4)), "пересадка состоялась");
        assert_eq!(layout.slots[1].participant_id, Some(pid(4)), "главное окно — у спикера");
        assert_eq!(layout.slots[3].participant_id, Some(pid(2)), "прежний жилец не пропал");
        // Все участники остались в раскладке ровно по одному разу.
        let mut seen: Vec<u128> = layout.slots.iter().filter_map(|s| s.participant_id).map(|p| p.as_u128()).collect();
        seen.sort_unstable();
        assert_eq!(seen, vec![1, 2, 3, 4]);
    }

    #[test]
    fn promotion_is_idempotent_and_ignores_absent_participants() {
        let mut layout = two_by_two();
        layout.slots[1].rect = Rect::new(0, 0, 1920, 810);
        assert!(promote_to_main_window(&mut layout, pid(4)));
        assert!(!promote_to_main_window(&mut layout, pid(4)), "повторная пересадка — не изменение");
        assert!(!promote_to_main_window(&mut layout, pid(99)), "участника нет в раскладке");
        assert!(!promote_to_main_window(&mut Layout::new(LayoutKind::Grid), pid(1)), "пустая раскладка");
    }
}
