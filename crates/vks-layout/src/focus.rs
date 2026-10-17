//! Фокусные раскладки: [`FocusLayout`] — один крупный фокус-тайл +
//! полоса мелких участников.
//!
//! Инварианты: фокус получает > 50% площади; остальные — мелкие;
//! сумма площадей == canvas (без пробелов и пересечений).

use crate::core::ParticipantId;
use crate::rect::{Canvas, Rect};

/// Генератор фокусных раскладок (focus + thumbnails).
pub struct FocusLayout;

impl FocusLayout {
    /// Вычисляет rect для каждого participant: фокус — крупная область
    /// (верхняя часть, ~75% canvas), остальные — полоса снизу (25%).
    ///
    /// Возвращает Vec<Rect> в порядке `participant_ids`. Если список
    /// пуст — пустой vec. Один participant → весь canvas.
    pub fn compute(canvas: &Canvas, participant_ids: &[ParticipantId], focus: ParticipantId) -> Vec<Rect> {
        if canvas.is_empty() || participant_ids.is_empty() {
            return Vec::new();
        }
        let n = participant_ids.len();
        if n == 1 {
            return vec![canvas.as_rect()];
        }
        // fallback: first participant as focus (caller should validate)
        let focus_idx: usize = participant_ids.iter().position(|p| *p == focus).unwrap_or_default();

        // Large area = top 75% of canvas; thumbnails = bottom 25% strip.
        let large_h = canvas.height * 75 / 100;
        let strip_h = canvas.height - large_h;

        let mut out = vec![Rect::new(0, 0, 0, 0); n];
        // large tile: full width, top
        out[focus_idx] = Rect::new(0, 0, canvas.width, large_h);

        // thumbnails: bottom strip, evenly split by count
        let others: Vec<usize> = (0..n).filter(|&i| i != focus_idx).collect();
        let m = others.len();
        for (k, &idx) in others.iter().enumerate() {
            let x = canvas.width * k as u32 / m as u32;
            let w = canvas.width / m as u32;
            out[idx] = Rect::new(x, large_h, w, strip_h);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn focus_single_fills_canvas() {
        let canvas = Canvas::new(1920, 1080);
        let rects = FocusLayout::compute(&canvas, &[pid(1)], pid(1));
        assert_eq!(rects.len(), 1);
        assert_eq!(rects[0], canvas.as_rect());
    }

    #[test]
    fn focus_gets_majority() {
        let canvas = Canvas::new(1920, 1080);
        let ids = vec![pid(1), pid(2), pid(3), pid(4)];
        let rects = FocusLayout::compute(&canvas, &ids, pid(2)); // focus = index 1
        assert_eq!(rects.len(), 4);
        assert_eq!(total_area(&rects), canvas.area());
        assert_no_overlap(&rects);
        assert_inside(&canvas, &rects);

        let focus_area = rects[1].area();
        assert!(focus_area > canvas.area() / 2, "focus must be >50%");

        // non-focus rects are smaller than focus
        for (i, r) in rects.iter().enumerate() {
            if i != 1 {
                assert!(r.area() < focus_area, "non-focus {i} not smaller");
            }
        }
    }

    #[test]
    fn focus_large_is_top_full_width() {
        let canvas = Canvas::new(1920, 1080);
        let ids = vec![pid(1), pid(2), pid(3)];
        let rects = FocusLayout::compute(&canvas, &ids, pid(3)); // focus = last
        assert_eq!(rects.len(), 3);
        let large = rects[2];
        assert_eq!(large.x, 0);
        assert_eq!(large.y, 0);
        assert_eq!(large.w, canvas.width);
        assert_eq!(large.h, canvas.height * 75 / 100);
        // thumbnails fill bottom strip
        let strip_top = canvas.height * 75 / 100;
        assert_eq!(rects[0].y, strip_top);
        assert_eq!(rects[1].y, strip_top);
    }

    #[test]
    fn focus_missing_focus_falls_back_to_first() {
        let canvas = Canvas::new(100, 100);
        let ids = vec![pid(1), pid(2)];
        let rects = FocusLayout::compute(&canvas, &ids, pid(999)); // not in list
        assert_eq!(rects.len(), 2);
        // fallback: index 0 is focus → top
        assert_eq!(rects[0].y, 0);
    }

    #[test]
    fn focus_empty_canvas_or_list() {
        let canvas = Canvas::new(0, 0);
        assert!(FocusLayout::compute(&canvas, &[pid(1)], pid(1)).is_empty());
        let canvas = Canvas::new(100, 100);
        assert!(FocusLayout::compute(&canvas, &[], pid(1)).is_empty());
    }
}
