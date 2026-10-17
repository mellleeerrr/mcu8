//! Сеточные раскладки: [`GridLayout`] — N тайлов в балансированную сетку.
//!
//! Алгоритм детерминированный: выбирается (rows, cols), минимизирующий
//! аспект-искажение тайлов; канвас делится без пробелов и пересечений.

use crate::rect::{Canvas, Rect};

/// Генератор сеточных раскладок (presets: 2x2, 3x3, …).
pub struct GridLayout;

/// Выбирает (rows, cols) для N тайлов, минимизируя отклонение асpects
/// тайлов от асpects канваса. Среди равных искажений предпочитает
/// меньшее число строк (больше колонок — ландшафт).
fn best_rows_cols(n: usize, canvas: &Canvas) -> (usize, usize) {
    if n <= 1 {
        return (1, 1);
    }
    let cw = f64::from(canvas.width);
    let ch = f64::from(canvas.height);
    let canvas_aspect = cw / ch;

    let mut best: Option<(f64, usize, usize)> = None;
    for r in 1..=n {
        let c = n.div_ceil(r);
        let tile_aspect = (cw / c as f64) / (ch / r as f64);
        let distortion = (tile_aspect / canvas_aspect - 1.0).abs();
        // prefer fewer rows (more cols) on equal distortion
        let better = match best {
            None => true,
            Some((bd, _br, _bc)) if distortion < bd - 1e-9 => true,
            Some((bd, br, _bc)) if distortion < bd + 1e-9 => r < br,
            _ => false,
        };
        if better {
            best = Some((distortion, r, c));
        }
    }
    let (_, r, c) = best.expect("n >= 1 yields at least (1,1)");
    (r, c)
}

/// Строит N rect, покрывающих canvas сеткой (rows x cols), заполняя
/// строки слева направо, сверху вниз. Каждый ряд занимает всю ширину
/// (running cursor → сумма ширин ряда == canvas.width); высота ряда —
/// с остатком на последнем ряду (сумма высот == canvas.height). Последний
/// ряд (если n не кратно cols) содержит меньше тайлов. Пробелов и
/// пересечений нет; `total area == canvas.area`.
fn grid_rects(n: usize, canvas: &Canvas) -> Vec<Rect> {
    let (rows, cols) = best_rows_cols(n, canvas);
    let (w, h) = (canvas.width, canvas.height);

    // how many tiles each row holds (full rows = cols, last row = remainder)
    let mut row_counts = Vec::with_capacity(rows);
    let mut remaining = n;
    for _ in 0..rows {
        let count = (cols.min(remaining)) as u32;
        row_counts.push(count);
        remaining -= count as usize;
    }

    // row heights: uniform division, remainder to the last row
    let base_h = if rows > 0 { h / rows as u32 } else { 0 };
    let mut row_heights = Vec::with_capacity(rows);
    let mut y_cursor = 0u32;
    for r in 0..rows {
        let tile_h = if r + 1 == rows { h.saturating_sub(y_cursor) } else { base_h };
        row_heights.push(tile_h);
        y_cursor = y_cursor.saturating_add(tile_h);
    }

    let mut out = Vec::with_capacity(n);
    let mut y = 0u32;
    for (r, &count) in row_counts.iter().enumerate() {
        let row_h = row_heights[r];
        let base_w = if count > 0 { w / count } else { 0 };
        let mut x_cursor = 0u32;
        for c in 0..count {
            let tile_w = if c + 1 == count {
                w.saturating_sub(x_cursor) // last tile in the row takes the remainder
            } else {
                base_w
            };
            out.push(Rect::new(x_cursor, y, tile_w, row_h));
            x_cursor = x_cursor.saturating_add(tile_w);
        }
        y = y.saturating_add(row_h);
    }
    out
}

impl GridLayout {
    /// Вычисляет N rect, покрывающих canvas сеткой.
    ///
    /// n = 1 → весь canvas. n = 2 → две половины (вдоль ширины).
    /// n = 4 → 2x2. И т.д. Детерминировано.
    pub fn compute(canvas: &Canvas, n_slots: usize) -> Vec<Rect> {
        if canvas.is_empty() || n_slots == 0 {
            return Vec::new();
        }
        grid_rects(n_slots, canvas)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
            assert!(r.x + r.w <= canvas.width && r.y + r.h <= canvas.height, "rect {r:?} outside canvas");
        }
    }

    #[test]
    fn grid_single_fills_canvas() {
        let canvas = Canvas::new(1920, 1080);
        let rects = GridLayout::compute(&canvas, 1);
        assert_eq!(rects.len(), 1);
        assert_eq!(rects[0], canvas.as_rect());
    }

    #[test]
    fn grid_two_halves() {
        let canvas = Canvas::new(1920, 1080);
        let rects = GridLayout::compute(&canvas, 2);
        assert_eq!(rects.len(), 2);
        assert_eq!(total_area(&rects), canvas.area());
        assert_no_overlap(&rects);
        assert_inside(&canvas, &rects);
        // two halves side by side (landscape preference)
        assert_eq!(rects[0].x, 0);
        assert_eq!(rects[1].x, 960);
        assert_eq!(rects[0].w, 960);
        assert_eq!(rects[1].w, 960);
        assert_eq!(rects[0].y, 0);
        assert_eq!(rects[1].y, 0);
        assert_eq!(rects[0].h, 1080);
        assert_eq!(rects[1].h, 1080);
    }

    #[test]
    fn grid_four_is_2x2() {
        let canvas = Canvas::new(1920, 1080);
        let rects = GridLayout::compute(&canvas, 4);
        assert_eq!(rects.len(), 4);
        assert_eq!(total_area(&rects), canvas.area());
        assert_no_overlap(&rects);
        assert_inside(&canvas, &rects);
        // 2x2: each tile 960x540
        for r in &rects {
            assert_eq!(r.w, 960);
            assert_eq!(r.h, 540);
        }
    }

    #[test]
    fn grid_five_six_eight_nine_balanced() {
        for n in [5usize, 6, 8, 9, 13, 16, 25] {
            let canvas = Canvas::new(1920, 1080);
            let rects = GridLayout::compute(&canvas, n);
            assert_eq!(rects.len(), n, "n={n}");
            assert_eq!(total_area(&rects), canvas.area(), "n={n} full coverage");
            assert_no_overlap(&rects);
            assert_inside(&canvas, &rects);
        }
    }

    #[test]
    fn grid_nine_and_sixteen_uniform_row_heights() {
        let canvas = Canvas::new(1920, 1080);

        // n=9 (3x3): rows must be 360, 360, 360, not distorted 360, 540, 180 (L01)
        let rects_9 = GridLayout::compute(&canvas, 9);
        assert_eq!(rects_9.len(), 9);
        for r in &rects_9 {
            assert_eq!(r.h, 360, "9-slot tiles must have uniform height 360");
            assert_eq!(r.w, 640, "9-slot tiles must have uniform width 640");
        }

        // n=16 (4x4): rows must be 270, 270, 270, 270, no subtraction underflow (L01)
        let rects_16 = GridLayout::compute(&canvas, 16);
        assert_eq!(rects_16.len(), 16);
        for r in &rects_16 {
            assert_eq!(r.h, 270, "16-slot tiles must have uniform height 270");
            assert_eq!(r.w, 480, "16-slot tiles must have uniform width 480");
        }
    }

    #[test]
    fn grid_small_canvas_and_zero() {
        let canvas = Canvas::new(0, 0);
        assert!(GridLayout::compute(&canvas, 4).is_empty());
        let canvas = Canvas::new(100, 100);
        assert!(GridLayout::compute(&canvas, 0).is_empty());
    }

    #[test]
    fn grid_uneven_rows_fill_width() {
        // 5 tiles → 2 rows (3 + 2). Each row tiles must span full width.
        let canvas = Canvas::new(1000, 1000);
        let rects = GridLayout::compute(&canvas, 5);
        assert_eq!(rects.len(), 5);
        assert_eq!(total_area(&rects), canvas.area());
        assert_no_overlap(&rects);
        // row 0 (first 3) must cover y in [0, 500), row 1 (last 2) in [500,1000)
        for (i, r) in rects.iter().enumerate() {
            if i < 3 {
                assert_eq!(r.y, 0);
            } else {
                assert_eq!(r.y, 500);
            }
        }
    }
}
