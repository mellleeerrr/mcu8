//! Параметрические схемы раскладок для конструктора: семейства схем
//! `vks_grid_<n>` (равномерная сетка), `vks_pin_{l,r,t,b}_<n>`
//! (1 крупный спикер + модульная матрица C x R со сквозными рядами),
//! и `vks_dual_<n>` (2 крупных докладчика + сквозные ряды),
//! где n — число окон (включая крупные id).
//!
//! Окна стремятся к пропорции 16:9 (для малых N ячейка может быть не 16:9:
//! `vks_grid_2` — 640×720) и имеют единые оси выравнивания благодаря единой
//! базовой матрице C x R без сплющенных ячеек (L-wrap bug).
//!
//! Порт части `meeting-grid-layout` поддерживает дополнительные опции:
//! `g<px>` — зазор между окнами, `m<px>` — внешние поля (`vks_grid_g12_m24_9`,
//! `vks_pin_r_g8_7`, `vks_dual_12`). В имени схемы опции заданы для 720p.

use crate::preset::{CalculatedSlot, SlotRect};

/// Максимальное число окон конструктора (счётчик UI — до 100).
pub const MAX_WINDOWS: usize = 100;

/// Максимальное значение опций `g`/`m` (пиксели канваса 720p).
pub const MAX_OPTION: u32 = 64;

/// Куда прижат крупный участник.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinSide {
    /// Крупный слева.
    Left,
    /// Крупный справа.
    Right,
    /// Крупный сверху.
    Top,
    /// Крупный снизу.
    Bottom,
}

/// Дополнительные параметры раскладки:
/// зазор между окнами `g<px>` и внешние поля `m<px>` в пикселях холста 720p.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct GridOptions {
    /// Зазор между соседними окнами (`g<px>`), 0 — вплотную.
    pub gap: u32,
    /// Поле по периметру холста (`m<px>`), 0 — окна до края.
    pub margin: u32,
}

impl GridOptions {
    /// Параметры по умолчанию — канвас без полей и зазоров.
    pub const fn is_default(self) -> bool {
        self.gap == 0 && self.margin == 0
    }

    /// Сегменты id для непустых параметров (`_g12`, `_m24`).
    fn id_suffix(self) -> String {
        let mut suffix = String::new();
        if self.gap > 0 {
            suffix.push_str(&format!("_g{}", self.gap));
        }
        if self.margin > 0 {
            suffix.push_str(&format!("_m{}", self.margin));
        }
        suffix
    }
}

/// Разобранная параметрическая раскладка.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GridScheme {
    /// Число окон (включая крупные).
    pub count: usize,
    /// `None` — равномерная галерея (или dual), `Some(side)` — PIN.
    pub pin: Option<PinSide>,
    /// Двойной крупный докладчик (`vks_dual_*`).
    pub is_dual: bool,
    /// Зазоры/поля (по умолчанию нули).
    pub options: GridOptions,
}

impl PinSide {
    /// Токен id (`l`, `r`, `t`, `b`).
    pub const fn token(self) -> &'static str {
        match self {
            Self::Left => "l",
            Self::Right => "r",
            Self::Top => "t",
            Self::Bottom => "b",
        }
    }
}

impl GridScheme {
    /// Галерея без крупного окна.
    pub fn gallery(count: usize, options: GridOptions) -> Self {
        Self { count, pin: None, is_dual: false, options }
    }

    /// Раскладка с 1 крупным окном.
    pub fn pin(count: usize, side: PinSide, options: GridOptions) -> Self {
        Self { count, pin: Some(side), is_dual: false, options }
    }

    /// Раскладка с 2 крупными окнами.
    pub fn dual(count: usize, options: GridOptions) -> Self {
        Self { count, pin: None, is_dual: true, options }
    }

    /// Формирует id (`vks_grid_9`, `vks_pin_r_g8_9`, `vks_dual_12`).
    pub fn id(self) -> String {
        let base = if self.is_dual {
            "vks_dual".to_string()
        } else {
            match self.pin {
                None => "vks_grid".to_string(),
                Some(side) => format!("vks_pin_{}", side.token()),
            }
        };
        format!("{}{}_{}", base, self.options.id_suffix(), self.count)
    }
}

/// Id параметрической раскладки (`vks_grid_<n>`, `vks_pin_<side>_<n>`, `vks_dual_<n>`).
pub fn is_grid_preset(id: &str) -> bool {
    parse(id).is_some()
}

/// Число окон параметрической раскладки; `None` — не тот id.
pub fn slot_count(id: &str) -> Option<usize> {
    parse(id).map(|scheme| scheme.count)
}

/// Разбор id параметрической раскладки; `None` — не тот id.
pub fn parse(id: &str) -> Option<GridScheme> {
    let id = strip_variant(id);
    let (head, tail) = id.rsplit_once('_')?;
    let count = tail.parse::<usize>().ok().filter(|count| (1..=MAX_WINDOWS).contains(count))?;
    let tokens: Vec<&str> = head.split('_').collect();
    if tokens.first() != Some(&"vks") {
        return None;
    }
    let (pin, is_dual, options_start) = match *tokens.get(1)? {
        "grid" => (None, false, 2),
        "dual" => (None, true, 2),
        "pin" => {
            let side = match *tokens.get(2)? {
                "l" => PinSide::Left,
                "r" => PinSide::Right,
                "t" => PinSide::Top,
                "b" => PinSide::Bottom,
                _ => return None,
            };
            (Some(side), false, 3)
        }
        _ => return None,
    };
    let mut options = GridOptions::default();
    let mut seen_gap = false;
    let mut seen_margin = false;
    for token in &tokens[options_start..] {
        if let Some(value) = token.strip_prefix('g') {
            if seen_gap || seen_margin {
                return None;
            }
            seen_gap = true;
            options.gap = parse_option(value)?;
        } else {
            let value = token.strip_prefix('m')?;
            if seen_margin {
                return None;
            }
            seen_margin = true;
            options.margin = parse_option(value)?;
        }
    }
    Some(GridScheme { count, pin, is_dual, options })
}

fn parse_option(value: &str) -> Option<u32> {
    value.parse::<u32>().ok().filter(|value| (1..=MAX_OPTION).contains(value))
}

fn strip_variant(id: &str) -> &str {
    let Some(start) = id.rfind("_(") else {
        return id;
    };
    let variant = &id[start + 2..];
    let Some(digits) = variant.strip_suffix(')') else {
        return id;
    };
    if digits.parse::<u32>().is_ok() {
        &id[..start]
    } else {
        id
    }
}

/// Геометрия параметрической раскладки на холсте `width × height`.
pub fn geometry(id: &str, width: f64, height: f64) -> Option<Vec<CalculatedSlot>> {
    let scheme = parse(id)?;
    Some(geometry_for(scheme, width, height))
}

/// Геометрия по разобранной схеме.
pub fn geometry_for(scheme: GridScheme, width: f64, height: f64) -> Vec<CalculatedSlot> {
    let (width, height) = canvas(width, height);
    let (gap, margin) = scaled_options(scheme.options, width, height);
    let rects = if scheme.is_dual {
        dual(scheme.count, width, height, gap, margin)
    } else {
        match scheme.pin {
            None => gallery(scheme.count, width, height, gap, margin),
            Some(side) => pin(scheme.count, side, width, height, gap, margin),
        }
    };
    rects.into_iter().map(|rect| CalculatedSlot { rect, z_order: 0 }).collect()
}

fn scaled_options(options: GridOptions, width: f64, height: f64) -> (f64, f64) {
    if options.is_default() || height <= 0.0 {
        return (0.0, 0.0);
    }
    let factor = height / 720.0;
    let gap = options.gap as f64 * factor;
    let margin = (options.margin as f64 * factor).min(width.min(height) / 4.0);
    (gap, margin)
}

fn canvas(width: f64, height: f64) -> (f64, f64) {
    let valid = |value: f64| value.is_finite() && value > 0.0;
    if valid(width) && valid(height) {
        (width, height)
    } else {
        (1280.0, 720.0)
    }
}

fn get_grid_edges(start: f64, count: usize, cell_size: f64, gap: f64) -> Vec<f64> {
    (0..=count).map(|i| (start + i as f64 * (cell_size + gap)).round()).collect()
}

/// Равномерная сетка (галерея) с центрированием последнего неполного ряда.
fn gallery(count: usize, width: f64, height: f64, gap: f64, margin: f64) -> Vec<SlotRect> {
    let count = count.max(1);
    let inner_w = (width - 2.0 * margin).max(1.0);
    let inner_h = (height - 2.0 * margin).max(1.0);
    let gap = clamp_gap(gap, count, inner_w, inner_h);
    let best = (1..=count)
        .min_by(|&left, &right| {
            let cost = |rows: usize| gallery_cost(count, rows, inner_w, inner_h, gap);
            cost(left).total_cmp(&cost(right))
        })
        .unwrap_or(1);
    let cols = count.div_ceil(best);
    let cell_w = ((inner_w - (cols - 1) as f64 * gap) / cols as f64).max(1.0);
    let cell_h = ((inner_h - (best - 1) as f64 * gap) / best as f64).max(1.0);
    let row_edges = get_grid_edges(margin, best, cell_h, gap);
    let mut rects = Vec::with_capacity(count);
    for row in 0..best {
        let in_row = (count - row * cols).min(cols);
        let row_width = in_row as f64 * cell_w + in_row.saturating_sub(1) as f64 * gap;
        let row_left = margin + (inner_w - row_width) / 2.0;
        let col_edges = get_grid_edges(row_left, in_row, cell_w, gap);
        for col in 0..in_row {
            let x = col_edges[col];
            let y = row_edges[row];
            let w = (col_edges[col + 1] - x - gap).max(1.0);
            let h = (row_edges[row + 1] - y - gap).max(1.0);
            rects.push(SlotRect::new(x, y, w, h));
        }
    }
    rects
}

fn gallery_cost(count: usize, rows: usize, width: f64, height: f64, gap: f64) -> f64 {
    let rows = rows.max(1);
    let cols = count.div_ceil(rows);
    let cell_w = (width - (cols - 1) as f64 * gap) / cols as f64;
    let cell_h = (height - (rows - 1) as f64 * gap) / rows as f64;
    let aspect = cell_w / cell_h.max(1.0);
    // Cell-aspect deviation only: the golden set N=1..100 fixes this choice
    // (N=43 -> 7x7 with one empty window), so there is no waste weight knob.
    (aspect - 16.0 / 9.0).abs() * 100.0
}

/// Конфигурация модульной матрицы C x R для 1 крупного окна:
/// (cols, rows, main_w, main_h).
pub fn prominent_config(count: usize) -> (usize, usize, usize, usize) {
    if count <= 6 {
        (3, 3, 2, 2)
    } else if count <= 8 {
        (4, 4, 3, 3)
    } else if count <= 10 {
        (5, 5, 4, 4)
    } else if count <= 12 {
        (5, 4, 3, 3)
    }
    // 12 окон: 5x4, главный 3x3!
    else if count <= 13 {
        (4, 4, 2, 2)
    }
    // 13 окон эталон: 4x4, главный 2x2!
    else if count <= 17 {
        (5, 5, 3, 3)
    } else if count <= 22 {
        (5, 5, 2, 2)
    } else if count <= 28 {
        (6, 6, 3, 3)
    } else if count <= 33 {
        (6, 6, 2, 2)
    } else if count <= 38 {
        (7, 6, 2, 2)
    } else if count <= 46 {
        (7, 7, 2, 2)
    } else if count <= 53 {
        (8, 7, 2, 2)
    } else if count <= 61 {
        (8, 8, 2, 2)
    } else if count <= 73 {
        (9, 9, 3, 3)
    } else if count <= 82 {
        (10, 9, 3, 3)
    } else if count <= 92 {
        (10, 10, 3, 3)
    } else {
        (11, 10, 3, 3)
    }
}

/// Конфигурация модульной матрицы для 2 крупных окон:
/// (cols, rows, main_w, main_h).
pub fn dual_config(count: usize) -> (usize, usize, usize, usize) {
    if count <= 2 {
        (2, 1, 1, 1)
    } else if count <= 6 {
        (4, 3, 2, 2)
    } else if count <= 10 {
        (4, 4, 2, 2)
    } else if count <= 14 {
        (4, 5, 2, 2)
    } else if count <= 18 {
        (6, 4, 2, 2)
    } else if count <= 24 {
        (6, 5, 2, 2)
    } else if count <= 30 {
        (6, 6, 2, 2)
    } else if count <= 36 {
        (7, 6, 2, 2)
    } else if count <= 50 {
        (8, 7, 2, 2)
    } else if count <= 66 {
        (9, 8, 2, 2)
    } else if count <= 84 {
        (10, 9, 2, 2)
    } else {
        (11, 10, 2, 2)
    }
}

/// 1 Крупный спикер + модульная матрица C x R со сквозными рядами.
fn pin(count: usize, side: PinSide, width: f64, height: f64, gap: f64, margin: f64) -> Vec<SlotRect> {
    let count = count.max(1);
    let inner_w = (width - 2.0 * margin).max(1.0);
    let inner_h = (height - 2.0 * margin).max(1.0);
    let gap = clamp_gap(gap, count, inner_w, inner_h);
    if count == 1 {
        return vec![SlotRect::new(margin, margin, inner_w, inner_h)];
    }

    let (cols, rows, main_w, main_h) = prominent_config(count);
    let unit_w = (inner_w - (cols - 1) as f64 * gap) / cols as f64;
    let unit_h = (inner_h - (rows - 1) as f64 * gap) / rows as f64;
    let col_edges = get_grid_edges(margin, cols, unit_w, gap);
    let row_edges = get_grid_edges(margin, rows, unit_h, gap);

    let mut rects = Vec::with_capacity(count);

    match side {
        PinSide::Left => {
            rects.push(SlotRect::new(
                col_edges[0],
                row_edges[0],
                (col_edges[main_w] - col_edges[0] - gap).max(1.0),
                (row_edges[main_h] - row_edges[0] - gap).max(1.0),
            ));

            let mut right_slots = Vec::new();
            for r in 0..main_h {
                for c in main_w..cols {
                    right_slots.push((r, c));
                }
            }
            let mut remaining = count - 1;
            let use_right = remaining.min(right_slots.len());
            for &(r, c) in &right_slots[..use_right] {
                rects.push(SlotRect::new(
                    col_edges[c],
                    row_edges[r],
                    (col_edges[c + 1] - col_edges[c] - gap).max(1.0),
                    (row_edges[r + 1] - row_edges[r] - gap).max(1.0),
                ));
            }
            remaining -= use_right;

            let bottom_row_count = rows - main_h;
            for r_idx in 0..bottom_row_count {
                if remaining == 0 {
                    break;
                }
                let r_num = main_h + r_idx;
                let take_in_row = remaining.min(cols);
                let row_width = take_in_row as f64 * unit_w + take_in_row.saturating_sub(1) as f64 * gap;
                let row_offset_x = (inner_w - row_width) / 2.0;
                let b_col_edges = get_grid_edges(margin + row_offset_x, take_in_row, unit_w, gap);
                for c_idx in 0..take_in_row {
                    rects.push(SlotRect::new(
                        b_col_edges[c_idx],
                        row_edges[r_num],
                        (b_col_edges[c_idx + 1] - b_col_edges[c_idx] - gap).max(1.0),
                        (row_edges[r_num + 1] - row_edges[r_num] - gap).max(1.0),
                    ));
                }
                remaining -= take_in_row;
            }
        }
        PinSide::Right => {
            rects.push(SlotRect::new(
                col_edges[cols - main_w],
                row_edges[0],
                (col_edges[cols] - col_edges[cols - main_w] - gap).max(1.0),
                (row_edges[main_h] - row_edges[0] - gap).max(1.0),
            ));

            let left_cols = cols - main_w;
            let mut left_slots = Vec::new();
            for r in 0..main_h {
                for c in 0..left_cols {
                    left_slots.push((r, c));
                }
            }
            let mut remaining = count - 1;
            let use_left = remaining.min(left_slots.len());
            for &(r, c) in &left_slots[..use_left] {
                rects.push(SlotRect::new(
                    col_edges[c],
                    row_edges[r],
                    (col_edges[c + 1] - col_edges[c] - gap).max(1.0),
                    (row_edges[r + 1] - row_edges[r] - gap).max(1.0),
                ));
            }
            remaining -= use_left;

            let bottom_row_count = rows - main_h;
            for r_idx in 0..bottom_row_count {
                if remaining == 0 {
                    break;
                }
                let r_num = main_h + r_idx;
                let take_in_row = remaining.min(cols);
                let row_width = take_in_row as f64 * unit_w + take_in_row.saturating_sub(1) as f64 * gap;
                let row_offset_x = (inner_w - row_width) / 2.0;
                let b_col_edges = get_grid_edges(margin + row_offset_x, take_in_row, unit_w, gap);
                for c_idx in 0..take_in_row {
                    rects.push(SlotRect::new(
                        b_col_edges[c_idx],
                        row_edges[r_num],
                        (b_col_edges[c_idx + 1] - b_col_edges[c_idx] - gap).max(1.0),
                        (row_edges[r_num + 1] - row_edges[r_num] - gap).max(1.0),
                    ));
                }
                remaining -= take_in_row;
            }
        }
        PinSide::Top => {
            let main_col_offset = (cols - main_w) / 2;
            rects.push(SlotRect::new(
                col_edges[main_col_offset],
                row_edges[0],
                (col_edges[main_col_offset + main_w] - col_edges[main_col_offset] - gap).max(1.0),
                (row_edges[main_h] - row_edges[0] - gap).max(1.0),
            ));

            let mut top_side_slots = Vec::new();
            for r in 0..main_h {
                for c in 0..main_col_offset {
                    top_side_slots.push((r, c));
                }
                for c in (main_col_offset + main_w)..cols {
                    top_side_slots.push((r, c));
                }
            }
            let mut remaining = count - 1;
            let use_top_side = remaining.min(top_side_slots.len());
            for &(r, c) in &top_side_slots[..use_top_side] {
                rects.push(SlotRect::new(
                    col_edges[c],
                    row_edges[r],
                    (col_edges[c + 1] - col_edges[c] - gap).max(1.0),
                    (row_edges[r + 1] - row_edges[r] - gap).max(1.0),
                ));
            }
            remaining -= use_top_side;

            let bottom_row_count = rows - main_h;
            for r_idx in 0..bottom_row_count {
                if remaining == 0 {
                    break;
                }
                let r_num = main_h + r_idx;
                let take_in_row = remaining.min(cols);
                let row_width = take_in_row as f64 * unit_w + take_in_row.saturating_sub(1) as f64 * gap;
                let row_offset_x = (inner_w - row_width) / 2.0;
                let b_col_edges = get_grid_edges(margin + row_offset_x, take_in_row, unit_w, gap);
                for c_idx in 0..take_in_row {
                    rects.push(SlotRect::new(
                        b_col_edges[c_idx],
                        row_edges[r_num],
                        (b_col_edges[c_idx + 1] - b_col_edges[c_idx] - gap).max(1.0),
                        (row_edges[r_num + 1] - row_edges[r_num] - gap).max(1.0),
                    ));
                }
                remaining -= take_in_row;
            }
        }
        PinSide::Bottom => {
            let main_col_offset = (cols - main_w) / 2;
            let main_row_offset = rows - main_h;
            rects.push(SlotRect::new(
                col_edges[main_col_offset],
                row_edges[main_row_offset],
                (col_edges[main_col_offset + main_w] - col_edges[main_col_offset] - gap).max(1.0),
                (row_edges[rows] - row_edges[main_row_offset] - gap).max(1.0),
            ));

            let mut remaining = count - 1;
            for r in 0..main_row_offset {
                if remaining == 0 {
                    break;
                }
                let take_in_row = remaining.min(cols);
                let row_width = take_in_row as f64 * unit_w + take_in_row.saturating_sub(1) as f64 * gap;
                let row_offset_x = (inner_w - row_width) / 2.0;
                let b_col_edges = get_grid_edges(margin + row_offset_x, take_in_row, unit_w, gap);
                for c in 0..take_in_row {
                    rects.push(SlotRect::new(
                        b_col_edges[c],
                        row_edges[r],
                        (b_col_edges[c + 1] - b_col_edges[c] - gap).max(1.0),
                        (row_edges[r + 1] - row_edges[r] - gap).max(1.0),
                    ));
                }
                remaining -= take_in_row;
            }

            let mut bottom_side_slots = Vec::new();
            for r in main_row_offset..rows {
                for c in 0..main_col_offset {
                    bottom_side_slots.push((r, c));
                }
                for c in (main_col_offset + main_w)..cols {
                    bottom_side_slots.push((r, c));
                }
            }
            let use_bottom_side = remaining.min(bottom_side_slots.len());
            for &(r, c) in &bottom_side_slots[..use_bottom_side] {
                rects.push(SlotRect::new(
                    col_edges[c],
                    row_edges[r],
                    (col_edges[c + 1] - col_edges[c] - gap).max(1.0),
                    (row_edges[r + 1] - row_edges[r] - gap).max(1.0),
                ));
            }
        }
    }

    rects
}

/// 2 Крупных докладчика бок о бок сверху + сквозные ряды.
fn dual(count: usize, width: f64, height: f64, gap: f64, margin: f64) -> Vec<SlotRect> {
    let count = count.max(1);
    let inner_w = (width - 2.0 * margin).max(1.0);
    let inner_h = (height - 2.0 * margin).max(1.0);
    let gap = clamp_gap(gap, count, inner_w, inner_h);
    if count == 1 {
        return vec![SlotRect::new(margin, margin, inner_w, inner_h)];
    }
    if count == 2 {
        let half_w = ((inner_w - gap) / 2.0).round();
        return vec![
            SlotRect::new(margin, margin, half_w, inner_h),
            SlotRect::new(margin + half_w + gap, margin, inner_w - half_w - gap, inner_h),
        ];
    }

    let (cols, rows, main_w, main_h) = dual_config(count);
    let unit_w = (inner_w - (cols - 1) as f64 * gap) / cols as f64;
    let unit_h = (inner_h - (rows - 1) as f64 * gap) / rows as f64;
    let col_edges = get_grid_edges(margin, cols, unit_w, gap);
    let row_edges = get_grid_edges(margin, rows, unit_h, gap);

    let mut rects = Vec::with_capacity(count);

    // Главный 1 (слева сверху)
    rects.push(SlotRect::new(
        col_edges[0],
        row_edges[0],
        (col_edges[main_w] - col_edges[0] - gap).max(1.0),
        (row_edges[main_h] - row_edges[0] - gap).max(1.0),
    ));
    // Главный 2 (справа сверху)
    rects.push(SlotRect::new(
        col_edges[cols - main_w],
        row_edges[0],
        (col_edges[cols] - col_edges[cols - main_w] - gap).max(1.0),
        (row_edges[main_h] - row_edges[0] - gap).max(1.0),
    ));

    let mut remaining = count - 2;

    // Средние слоты между двумя главными
    let mut mid_slots = Vec::new();
    for r in 0..main_h {
        for c in main_w..(cols - main_w) {
            mid_slots.push((r, c));
        }
    }
    let use_mid = remaining.min(mid_slots.len());
    for &(r, c) in &mid_slots[..use_mid] {
        rects.push(SlotRect::new(
            col_edges[c],
            row_edges[r],
            (col_edges[c + 1] - col_edges[c] - gap).max(1.0),
            (row_edges[r + 1] - row_edges[r] - gap).max(1.0),
        ));
    }
    remaining -= use_mid;

    // Нижние сквозные ряды
    let bottom_row_count = rows - main_h;
    for r_idx in 0..bottom_row_count {
        if remaining == 0 {
            break;
        }
        let r_num = main_h + r_idx;
        let take_in_row = remaining.min(cols);
        let row_width = take_in_row as f64 * unit_w + take_in_row.saturating_sub(1) as f64 * gap;
        let row_offset_x = (inner_w - row_width) / 2.0;
        let b_col_edges = get_grid_edges(margin + row_offset_x, take_in_row, unit_w, gap);
        for c_idx in 0..take_in_row {
            rects.push(SlotRect::new(
                b_col_edges[c_idx],
                row_edges[r_num],
                (b_col_edges[c_idx + 1] - b_col_edges[c_idx] - gap).max(1.0),
                (row_edges[r_num + 1] - row_edges[r_num] - gap).max(1.0),
            ));
        }
        remaining -= take_in_row;
    }

    rects
}

fn clamp_gap(gap: f64, count: usize, inner_w: f64, inner_h: f64) -> f64 {
    if !gap.is_finite() || gap <= 0.0 {
        return 0.0;
    }
    let (cols, rows, _, _) = prominent_config(count);
    let max_axis = cols.max(rows) as f64;
    let smallest = inner_w.min(inner_h);
    if smallest - (max_axis - 1.0) * gap < max_axis * 10.0 {
        0.0
    } else {
        gap
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_понимает_все_семейства() {
        assert_eq!(
            parse("vks_grid_9"),
            Some(GridScheme { count: 9, pin: None, is_dual: false, options: GridOptions::default() })
        );
        assert_eq!(
            parse("vks_pin_r_8"),
            Some(GridScheme { count: 8, pin: Some(PinSide::Right), is_dual: false, options: GridOptions::default() })
        );
        assert_eq!(
            parse("vks_dual_12"),
            Some(GridScheme { count: 12, pin: None, is_dual: true, options: GridOptions::default() })
        );
        assert_eq!(
            parse("vks_grid_g12_m24_25"),
            Some(GridScheme { count: 25, pin: None, is_dual: false, options: GridOptions { gap: 12, margin: 24 } })
        );
        assert_eq!(
            parse("vks_pin_l_g8_7"),
            Some(GridScheme {
                count: 7,
                pin: Some(PinSide::Left),
                is_dual: false,
                options: GridOptions { gap: 8, margin: 0 }
            })
        );
        assert_eq!(
            parse("vks_grid_9_(2)"),
            Some(GridScheme { count: 9, pin: None, is_dual: false, options: GridOptions::default() })
        );
        assert_eq!(parse("vks_grid_9_g12"), None, "число окон обязано быть последним сегментом");
        assert_eq!(parse("vks_grid_g8_g8_9"), None, "дубль зазора");
        assert_eq!(parse("vks_grid_m8_g8_9"), None, "не канонический порядок опций");
        assert_eq!(parse("vks_grid_g0_9"), None, "нулевая опция не выводится в id");
        assert_eq!(parse("vks_grid_g999_9"), None, "зазор ограничен");
        assert_eq!(parse("vks_grid_0"), None);
        assert_eq!(parse("codian_01"), None);
        assert_eq!(parse("preset_equal_9"), None);
    }

    #[test]
    fn идентификаторы_круговые() {
        for count in 1..=36 {
            for (pin, is_dual) in [
                (None, false),
                (Some(PinSide::Left), false),
                (Some(PinSide::Right), false),
                (Some(PinSide::Top), false),
                (Some(PinSide::Bottom), false),
                (None, true),
            ] {
                for options in [
                    GridOptions::default(),
                    GridOptions { gap: 8, margin: 0 },
                    GridOptions { gap: 0, margin: 16 },
                    GridOptions { gap: 12, margin: 24 },
                ] {
                    let scheme = GridScheme { count, pin, is_dual, options };
                    assert_eq!(parse(&scheme.id()), Some(scheme));
                }
            }
        }
    }

    #[test]
    fn галерея_предпочитает_16_9_а_не_заполнение_сетки() {
        // N=43: 7×7 с неполным рядом даёт ячейку 16:9; 5×9 «заполняет» сетку,
        // но растягивает окна. Приоритет — форма кадра, а не пустые ячейки.
        assert!(
            super::gallery_cost(43, 7, 1280.0, 720.0, 0.0) < super::gallery_cost(43, 5, 1280.0, 720.0, 0.0),
            "16:9-ячейки важнее отсутствия пустых окон"
        );
    }

    #[test]
    fn slot_count_совпадает_с_числом_окон() {
        assert_eq!(slot_count("vks_grid_g8_m16_5"), Some(5));
        assert_eq!(slot_count("vks_dual_12"), Some(12));
        assert_eq!(slot_count("codian_01"), None);
    }
}
