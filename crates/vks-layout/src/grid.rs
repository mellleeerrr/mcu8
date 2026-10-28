//! Сеточные раскладки: [`GridLayout`] — N тайлов в балансированную сетку.
//!
//! Алгоритм детерминированный: выбирается (rows, cols), минимизирующий
//! аспект-искажение тайлов. Полные ряды делят ширину без пробелов и
//! пересечений; неполный последний ряд сохраняет ширину колонки и
//! центрируется (по краям остаются поля).

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

/// Строит N rect сеткой (rows x cols), заполняя строки слева направо,
/// сверху вниз. Ширина тайла — ширина колонки полной сетки (`w / cols`),
/// поэтому неполный последний ряд не растягивается на всю ширину, а
/// центрируется (`row_left = (w - count*base_w) / 2`). Полные ряды занимают
/// всю ширину (остаток деления отдан последнему тайлу); высота ряда — с
/// остатком на последнем ряду. Пересечений нет; `total area == canvas.area`
/// только когда `n` кратно `cols` (у неполного ряда остаются поля).
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

    let base_w = w.checked_div(cols as u32).unwrap_or(0);
    let mut out = Vec::with_capacity(n);
    let mut y = 0u32;
    for (r, &count) in row_counts.iter().enumerate() {
        let row_h = row_heights[r];
        // Холст уже сетки (base_w == 0): центрировать нечем — сохраняем прежний
        // режим «ряд делит свою ширину», иначе тайлы получили бы нулевую ширину.
        let stretch_row = base_w == 0;
        let cell_w = if stretch_row { w.checked_div(count).unwrap_or(0) } else { base_w };
        // Полный ряд — от левого края (последний тайл добирает остаток деления);
        // неполный — по центру, тайлы шириной полной колонки (без растяжения).
        let full_row = count as usize == cols;
        let row_left = if full_row || stretch_row { 0 } else { w.saturating_sub(base_w * count) / 2 };
        let mut x_cursor = row_left;
        for c in 0..count {
            let tile_w = if (full_row || stretch_row) && c + 1 == count {
                w.saturating_sub(x_cursor) // last tile in the stretched row takes the remainder
            } else {
                cell_w
            };
            out.push(Rect::new(x_cursor, y, tile_w, row_h));
            x_cursor = x_cursor.saturating_add(tile_w);
        }
        y = y.saturating_add(row_h);
    }
    out
}

impl GridLayout {
    /// Вычисляет N rect, образующих сетку на canvas.
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
            for rect in &rects {
                assert!(rect.w > 0 && rect.h > 0, "n={n}: пустой тайл {rect:?}");
            }
            let (_, cols) = best_rows_cols(n, &canvas);
            if n % cols == 0 {
                assert_eq!(total_area(&rects), canvas.area(), "n={n}: полная сетка покрывает холст");
            } else {
                assert!(total_area(&rects) < canvas.area(), "n={n}: неполный ряд оставляет поля");
            }
            assert_no_overlap(&rects);
            assert_inside(&canvas, &rects);
        }
    }

    #[test]
    fn grid_incomplete_last_row_is_centered_without_stretching() {
        // S6 DoD: N ≡ 1 (mod cols) на трёх холстах — последний тайл шириной
        // колонки, по центру; полные ряды по-прежнему покрывают ширину.
        for (width, height, n, rows, cols) in [
            (1920u32, 1080u32, 43usize, 7usize, 7u32),
            (1281, 721, 43, 7, 7),
            (1000, 1000, 43, 7, 7),
            (1920, 1080, 57, 8, 8),
            (1000, 1000, 73, 9, 9),
        ] {
            let canvas = Canvas::new(width, height);
            let (actual_rows, actual_cols) = best_rows_cols(n, &canvas);
            assert_eq!((actual_rows, actual_cols as usize), (rows, cols as usize), "N={n} @ {width}x{height}");
            let rects = GridLayout::compute(&canvas, n);
            assert_eq!(rects.len(), n, "N={n}: число тайлов");
            for rect in &rects {
                assert!(rect.w > 0 && rect.h > 0, "N={n}: пустой тайл {rect:?}");
                assert!(rect.x + rect.w <= width && rect.y + rect.h <= height, "N={n}: тайл за холстом");
            }
            // Полный ряд (тайлы 0..cols) занимает всю ширину.
            assert_eq!(rects[0].x, 0, "N={n}: полный ряд слева");
            let full_row_right = rects[cols as usize - 1].x + rects[cols as usize - 1].w;
            assert_eq!(full_row_right, width, "N={n}: полный ряд покрывает ширину");
            // Неполный последний ряд: один тайл шириной колонки, по центру.
            let last_row: Vec<_> = rects.iter().filter(|rect| rect.y + rect.h >= height).collect();
            assert_eq!(last_row.len(), n - (rows - 1) * cols as usize, "N={n}: последний ряд неполный");
            let cell_w = width / cols;
            assert_eq!(last_row[0].w, cell_w, "N={n}: ширина колонки без растяжения");
            assert_eq!(last_row[0].x, (width - cell_w) / 2, "N={n}: последний тайл центрирован");
            assert_inside(&canvas, &rects);
            assert_no_overlap(&rects);
        }
    }

    #[test]
    fn tiny_canvas_keeps_positive_tile_width() {
        // Регрессия: холст уже сетки (base_w == 0) не должен давать нулевую
        // ширину — используется прежний режим «ряд делит свою ширину».
        for (canvas, n) in [(Canvas::new(2, 10), 5usize), (Canvas::new(6, 6), 43), (Canvas::new(3, 3), 4)] {
            let rects = GridLayout::compute(&canvas, n);
            assert_eq!(rects.len(), n);
            for rect in &rects {
                assert!(rect.x + rect.w <= canvas.width && rect.y + rect.h <= canvas.height);
            }
            // Неполный последний ряд не регрессирует в нулевую ширину: он
            // сохраняет прежний режим «ряд делит свою ширину».
            let last_row: Vec<_> = rects.iter().filter(|rect| rect.y + rect.h >= canvas.height).collect();
            for rect in &last_row {
                assert!(rect.w > 0, "canvas {canvas:?}, n={n}: нулевая ширина последнего ряда {rect:?}");
            }
            assert_no_overlap(&rects);
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
    fn grid_uneven_rows_center_incomplete_row() {
        // 5 tiles → 2 ряда (3 + 2): полный ряд покрывает ширину, неполный
        // центрируется тайлами шириной колонки (поля по краям, без растяжения).
        let canvas = Canvas::new(1000, 1000);
        let rects = GridLayout::compute(&canvas, 5);
        assert_eq!(rects.len(), 5);
        assert_no_overlap(&rects);
        assert_inside(&canvas, &rects);
        for (i, r) in rects.iter().enumerate() {
            if i < 3 {
                assert_eq!(r.y, 0, "полный ряд сверху");
                assert_eq!(r.h, 500);
            } else {
                assert_eq!(r.y, 500, "неполный ряд снизу");
                assert_eq!(r.h, 500);
            }
        }
        let cell_w = canvas.width / 3;
        let last_left = (canvas.width - 2 * cell_w) / 2;
        assert_eq!(rects[3].x, last_left, "неполный ряд центрирован");
        assert_eq!(rects[3].w, cell_w);
        assert_eq!(rects[4].x, last_left + cell_w);
        assert_eq!(rects[4].w, cell_w);
        // Полный ряд по-прежнему покрывает ширину (остаток деления — последнему тайлу).
        assert_eq!(rects[0].x, 0);
        assert_eq!(rects[0].w, cell_w);
        assert_eq!(rects[2].x + rects[2].w, canvas.width);
    }
}
