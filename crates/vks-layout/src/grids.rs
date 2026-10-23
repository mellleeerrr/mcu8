//! Конструктор «Схема расположения участников»: параметрические раскладки
//! `vks_grid_<n>` (равномерная галерея) и `vks_pin_{l,r,t,b}_<n>`
//! (крупный участник + лента), n — число окон (последний сегмент id).
//!
//! Идея: id раскладки полностью определяет её геометрию — не нужны ни реестр,
//! ни хранение координат в БД. Вендорский UI при открытии раскладки зовёт
//! `calculate(preset)` и получает ту же геометрию, что и MCU при применении.
//! Алгоритм — framework-agnostic порт `meeting-grid-layout-core` (галерея:
//! подбор числа рядов; pin: крупный участник + лента в 1/3 канваса), без
//! анимаций и пагинации: серверной геометрии они не нужны.
//!
//! Инварианты (проверяются в `tests/grids.rs`):
//! * окон ровно `n`;
//! * окна не пересекаются и ровно покрывают канвас без щелей — иначе в
//!   композите появятся тёмные швы;
//! * все координаты конечны и лежат внутри канваса.
//!
//! Пикселизация — целочисленные границы (`floor`), как у вендорных пресетов:
//! соседние ячейки стыкуются край-в-край.

use crate::preset::{CalculatedSlot, SlotRect};

/// Куда прижат крупный участник (лента занимает противоположную треть).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinSide {
    /// Крупный слева, лента справа.
    Left,
    /// Крупный справа, лента слева.
    Right,
    /// Крупный сверху, лента снизу.
    Top,
    /// Крупный снизу, лента сверху.
    Bottom,
}

/// Разобранная параметрическая раскладка.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GridScheme {
    /// Число окон (включая крупное).
    pub count: usize,
    /// `None` — равномерная галерея, `Some(side)` — PIN + лента.
    pub pin: Option<PinSide>,
}

impl PinSide {
    /// Сегмент id (`l`, `r`, `t`, `b`).
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
    /// Канонический id (`vks_grid_9`, `vks_pin_r_9`).
    pub fn id(self) -> String {
        match self.pin {
            None => format!("vks_grid_{}", self.count),
            Some(side) => format!("vks_pin_{}_{}", side.token(), self.count),
        }
    }
}

/// Id параметрической раскладки (`vks_grid_<n>` / `vks_pin_<side>_<n>`).
pub fn is_grid_preset(id: &str) -> bool {
    parse(id).is_some()
}

/// Разбирает id параметрической раскладки; `None` — чужой id.
///
/// Число окон — последний сегмент: вендорский UI определяет размер раскладки
/// по хвосту id (`Iee`), поэтому подпись «N окон» в редакторе совпадает.
pub fn parse(id: &str) -> Option<GridScheme> {
    let (head, tail) = id.rsplit_once('_')?;
    let count = tail.parse::<usize>().ok().filter(|count| (1..=64).contains(count))?;
    if head == "vks_grid" {
        return Some(GridScheme { count, pin: None });
    }
    let side = match head {
        "vks_pin_l" => PinSide::Left,
        "vks_pin_r" => PinSide::Right,
        "vks_pin_t" => PinSide::Top,
        "vks_pin_b" => PinSide::Bottom,
        _ => return None,
    };
    Some(GridScheme { count, pin: Some(side) })
}

/// Геометрия параметрической раскладки на канвасе `width × height`.
///
/// `None` — для чужого id. Наложенных PIP-окон нет: PIN-режим — отдельные
/// непересекающиеся зоны, поэтому все слоты идут с `z_order = 0`.
pub fn geometry(id: &str, width: f64, height: f64) -> Option<Vec<CalculatedSlot>> {
    let scheme = parse(id)?;
    Some(geometry_for(scheme, width, height))
}

/// Геометрия по разобранной схеме (используется вместе с [`parse`]).
pub fn geometry_for(scheme: GridScheme, width: f64, height: f64) -> Vec<CalculatedSlot> {
    let (width, height) = canvas(width, height);
    let rects = match scheme.pin {
        None => gallery(scheme.count, width, height),
        Some(side) => pin(scheme.count, side, width, height),
    };
    rects.into_iter().map(|rect| CalculatedSlot { rect, z_order: 0 }).collect()
}

fn canvas(width: f64, height: f64) -> (f64, f64) {
    let width = if width.is_finite() && width > 0.0 { width } else { 1280.0 };
    let height = if height.is_finite() && height > 0.0 { height } else { 720.0 };
    (width, height)
}

/// Границы `index`-й полосы из `parts` равных частей размера `size` (пиксели).
fn band(index: usize, parts: usize, size: f64) -> (f64, f64) {
    let parts = parts.max(1) as f64;
    let left = (size * index as f64 / parts).floor();
    let right = (size * (index + 1) as f64 / parts).floor();
    (left, right)
}

/// Окно `(x, y, width, height)` из границ полос.
fn cell(x: (f64, f64), y: (f64, f64)) -> SlotRect {
    SlotRect::new(x.0, y.0, (x.1 - x.0).max(1.0), (y.1 - y.0).max(1.0))
}

/// Галерея: подбирает число рядов, при котором ячейка ближе всего к 16:9
/// (с учётом простоя неполного ряда), и раздаёт неполный ряд на всю ширину.
fn gallery(count: usize, width: f64, height: f64) -> Vec<SlotRect> {
    let count = count.max(1);
    let best = (1..=count).min_by_key(|&rows| gallery_cost(count, rows, width, height)).unwrap_or(1);
    let cols = count.div_ceil(best);
    let mut rects = Vec::with_capacity(count);
    for row in 0..best {
        let in_row = (count - row * cols).min(cols);
        for col in 0..in_row {
            rects.push(cell(band(col, in_row, width), band(row, best, height)));
        }
    }
    rects
}

/// Стоимость варианта «`rows` рядов»: отклонение ячейки от 16:9 + простой.
fn gallery_cost(count: usize, rows: usize, width: f64, height: f64) -> u64 {
    let cols = count.div_ceil(rows);
    let cell_w = width / cols as f64;
    let cell_h = height / rows as f64;
    let aspect = (cell_w / cell_h - 16.0 / 9.0).abs() * 100.0;
    let waste = (cols * rows - count) as f64 * 20.0;
    (aspect + waste) as u64
}

/// PIN + лента: крупный участник занимает 2/3 канваса, лента — треть;
/// внутри ленты — сетка, ближайшая к 16:9.
fn pin(count: usize, side: PinSide, width: f64, height: f64) -> Vec<SlotRect> {
    let count = count.max(1);
    if count == 1 {
        return vec![cell((0.0, width), (0.0, height))];
    }
    let horizontal = matches!(side, PinSide::Top | PinSide::Bottom);
    let (main, strip) = if horizontal {
        let main = (height * 2.0 / 3.0).floor();
        (main, height - main)
    } else {
        let main = (width * 2.0 / 3.0).floor();
        (main, width - main)
    };
    let strip_count = count - 1;
    let cols =
        (1..=strip_count).min_by_key(|&cols| strip_cost(strip_count, cols, main, strip, horizontal)).unwrap_or(1);
    let rows = strip_count.div_ceil(cols);
    let (strip_origin, pin_rect) = match side {
        PinSide::Left => (main, cell((0.0, main), (0.0, height))),
        PinSide::Right => (0.0, cell((strip, width), (0.0, height))),
        PinSide::Top => (main, cell((0.0, width), (0.0, main))),
        PinSide::Bottom => (0.0, cell((0.0, width), (strip, height))),
    };
    let mut rects = Vec::with_capacity(count);
    rects.push(pin_rect);
    for row in 0..rows {
        let in_row = (strip_count - row * cols).min(cols);
        for col in 0..in_row {
            let (along0, along1) = if horizontal {
                let (start, end) = band(row, rows, strip);
                (strip_origin + start, strip_origin + end)
            } else {
                band(row, rows, height)
            };
            let (across0, across1) = if horizontal {
                band(col, in_row, width)
            } else {
                let (start, end) = band(col, in_row, strip);
                (strip_origin + start, strip_origin + end)
            };
            rects.push(SlotRect::new(across0, along0, (across1 - across0).max(1.0), (along1 - along0).max(1.0)));
        }
    }
    rects
}

/// Стоимость сетки ленты: отклонение ячейки от 16:9 + простой.
fn strip_cost(strip_count: usize, cols: usize, main: f64, strip: f64, horizontal: bool) -> u64 {
    let rows = strip_count.div_ceil(cols);
    let (cell_w, cell_h) =
        if horizontal { (main / cols as f64, strip / rows as f64) } else { (strip / cols as f64, main / rows as f64) };
    let aspect = (cell_w / cell_h - 16.0 / 9.0).abs() * 100.0;
    let waste = (cols * rows - strip_count) as f64 * 20.0;
    (aspect + waste) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_понимает_оба_семейства() {
        assert_eq!(parse("vks_grid_9"), Some(GridScheme { count: 9, pin: None }));
        assert_eq!(parse("vks_pin_r_8"), Some(GridScheme { count: 8, pin: Some(PinSide::Right) }));
        assert_eq!(parse("vks_grid_0"), None);
        assert_eq!(parse("codian_01"), None);
        assert_eq!(parse("preset_equal_9"), None);
    }

    #[test]
    fn идентификаторы_круговые() {
        for count in 1..=36 {
            for pin in [None, Some(PinSide::Left), Some(PinSide::Right), Some(PinSide::Top), Some(PinSide::Bottom)] {
                let scheme = GridScheme { count, pin };
                assert_eq!(parse(&scheme.id()), Some(scheme));
            }
        }
    }
}
