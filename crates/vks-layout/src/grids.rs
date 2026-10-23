//! Конструктор «Схема расположения участников»: параметрические раскладки
//! `vks_grid_<n>` (равномерная галерея) и `vks_pin_{l,r,t,b}_<n>`
//! (крупный участник + лента 16:9 миниатюр), n — число окон (последний сегмент id).
//!
//! Опции порта `meeting-grid-layout` кодируются перед числом окон:
//! `g<px>` — зазор между окнами, `m<px>` — внешние поля (`vks_grid_g12_m24_9`,
//! `vks_pin_r_g8_7`). Единицы опций — пиксели холста 720p; при рендере на другом
//! холсте масштабируются по высоте (`scaled_options`), чтобы доля полей не
//! зависела от разрешения. Канонический порядок — `g`, затем `m`, каждый не
//! более одного раза; нулевые опции не выводятся.
//!
//! Идея: id раскладки полностью определяет её геометрию — не нужны ни реестр,
//! ни хранение координат в БД. Вендорский UI при открытии раскладки зовёт
//! `calculate(preset)` и получает ту же геометрию, что и MCU при применении.
//!
//! Модель библиотеки: галерея — равные ячейки (последний неполный ряд
//! центрирован), pin — крупный участник и лента миниатюр с сохранением 16:9.
//! Не переносится в пиксельный композит: `pinOnly` (это страницы состояния
//! комнаты), пагинация/«+N more» (состояние комнаты, не свойство раскладки),
//! spring-анимации; crossfade композитора уже есть отдельно.
//!
//! Инварианты (проверяются в `tests/grids.rs`):
//! * окон ровно `n`;
//! * окна внутри холста, не пересекаются, соблюдают поля;
//! * у галереи все ячейки равны, у pin миниатюры 16:9 и не крупнее крупного;
//! * координаты конечны; целочисленный разрез не даёт щелей/наложений на 1 px.

use crate::preset::{CalculatedSlot, SlotRect};

/// Ограничение числа окон (и значений `g`/`m`) в параметрической раскладке.
pub const MAX_WINDOWS: usize = 64;

/// Куда прижат крупный участник (лента занимает противоположную сторону).
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

/// Дополнительные параметры раскладки (порт `meeting-grid-layout`):
/// зазор между окнами `g<px>` и внешние поля `m<px>` в пикселях холста 720p.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct GridOptions {
    /// Зазор между соседними окнами (`g<px>`), 0 — вплотную (швов нет).
    pub gap: u32,
    /// Поле по периметру холста (`m<px>`), 0 — окна до края.
    pub margin: u32,
}

impl GridOptions {
    /// Параметры по умолчанию — канвас без полей и зазоров (бесшовный композит).
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
    /// Число окон (включая крупное).
    pub count: usize,
    /// `None` — равномерная галерея, `Some(side)` — PIN + лента.
    pub pin: Option<PinSide>,
    /// Зазоры/поля (по умолчанию нули).
    pub options: GridOptions,
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
    /// Канонический id (`vks_grid_9`, `vks_pin_r_g8_9`, `vks_grid_g12_m24_25`).
    pub fn id(self) -> String {
        let base = match self.pin {
            None => "vks_grid".to_string(),
            Some(side) => format!("vks_pin_{}", side.token()),
        };
        format!("{}{}_{}", base, self.options.id_suffix(), self.count)
    }
}

/// Id параметрической раскладки (`vks_grid_<n>` / `vks_pin_<side>_<n>`).
pub fn is_grid_preset(id: &str) -> bool {
    parse(id).is_some()
}

/// Число окон параметрической раскладки; `None` — чужой id.
pub fn slot_count(id: &str) -> Option<usize> {
    parse(id).map(|scheme| scheme.count)
}

/// Разбирает id параметрической раскладки; `None` — чужой id.
///
/// Число окон — последний сегмент: вендорский UI определяет размер раскладки
/// по хвосту id (`Iee`), поэтому подпись «N окон» в редакторе совпадает.
/// Вариант `_(2)` в хвосте (вендорский суффикс копии) срезается до разбора.
pub fn parse(id: &str) -> Option<GridScheme> {
    let id = strip_variant(id);
    let (head, tail) = id.rsplit_once('_')?;
    let count = tail.parse::<usize>().ok().filter(|count| (1..=MAX_WINDOWS).contains(count))?;
    let tokens: Vec<&str> = head.split('_').collect();
    if tokens.first() != Some(&"vks") {
        return None;
    }
    let (pin, options_start) = match *tokens.get(1)? {
        "grid" => (None, 2),
        "pin" => {
            let side = match *tokens.get(2)? {
                "l" => PinSide::Left,
                "r" => PinSide::Right,
                "t" => PinSide::Top,
                "b" => PinSide::Bottom,
                _ => return None,
            };
            (Some(side), 3)
        }
        _ => return None,
    };
    let mut options = GridOptions::default();
    let mut seen_gap = false;
    let mut seen_margin = false;
    for token in &tokens[options_start..] {
        if let Some(value) = token.strip_prefix('g') {
            if seen_gap || seen_margin {
                return None; // дубль или не канонический порядок
            }
            seen_gap = true;
            options.gap = parse_option(value)?;
        } else if let Some(value) = token.strip_prefix('m') {
            if seen_margin {
                return None; // дубль
            }
            seen_margin = true;
            options.margin = parse_option(value)?;
        } else {
            return None;
        }
    }
    Some(GridScheme { count, pin, options })
}

/// Значение опции `g`/`m`: 1..=64 (нулевые опции в id не выводятся).
fn parse_option(value: &str) -> Option<u32> {
    value.parse::<u32>().ok().filter(|value| (1..=MAX_WINDOWS as u32).contains(value))
}

/// Срезает вендорский суффикс варианта `_(2)` (копия раскладки).
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
    let (gap, margin) = scaled_options(scheme.options, width, height);
    let rects = match scheme.pin {
        None => gallery(scheme.count, width, height, gap, margin),
        Some(side) => pin(scheme.count, side, width, height, gap, margin),
    };
    rects.into_iter().map(|rect| CalculatedSlot { rect, z_order: 0 }).collect()
}

/// Пиксели опций на конкретном холсте: id хранит значения для 720p, поэтому
/// масштаб — по высоте; поля ограничены четвертью меньшей стороны.
fn scaled_options(options: GridOptions, width: f64, height: f64) -> (f64, f64) {
    if options.is_default() || height <= 0.0 {
        return (0.0, 0.0);
    }
    let factor = height / 720.0;
    let gap = options.gap as f64 * factor;
    let margin = (options.margin as f64 * factor).min(width.min(height) / 4.0);
    (gap, margin)
}

/// Канвас: неположительные/нечисловые размеры дают безопасный 1280×720.
fn canvas(width: f64, height: f64) -> (f64, f64) {
    let valid = |value: f64| value.is_finite() && value > 0.0;
    if valid(width) && valid(height) {
        (width, height)
    } else {
        (1280.0, 720.0)
    }
}

/// Равномерная галерея: подбор числа рядов по близости ячейки к 16:9 с учётом
/// зазоров, равные ячейки, последний неполный ряд центрирован.
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
    // Целочисленный разрез по краям: соседние ячейки стыкуются без щелей.
    let row_edges: Vec<f64> = (0..=best).map(|row| (margin + row as f64 * (cell_h + gap)).round()).collect();
    let mut rects = Vec::with_capacity(count);
    for row in 0..best {
        let in_row = (count - row * cols).min(cols);
        let row_width = in_row as f64 * cell_w + in_row.saturating_sub(1) as f64 * gap;
        let row_left = margin + (inner_w - row_width) / 2.0;
        let col_edges: Vec<f64> = (0..=in_row).map(|col| (row_left + col as f64 * (cell_w + gap)).round()).collect();
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

/// Стоимость сетки `rows × ceil(count/rows)` (меньше — ближе к 16:9 и плотнее).
fn gallery_cost(count: usize, rows: usize, width: f64, height: f64, gap: f64) -> f64 {
    let rows = rows.max(1);
    let cols = count.div_ceil(rows);
    let cell_w = (width - (cols - 1) as f64 * gap) / cols as f64;
    let cell_h = (height - (rows - 1) as f64 * gap) / rows as f64;
    let aspect = cell_w / cell_h.max(1.0);
    let deviation = (aspect - 16.0 / 9.0).abs() * 100.0;
    let waste = (cols * rows - count) as f64 * 20.0;
    deviation + waste
}

/// Параметры ленты: смещение по разделяющей оси, число столбцов/рядов, размер
/// миниатюры 16:9 и полный размер ленты.
struct StripLayout {
    /// Начало ленты по разделяющей оси (x для Left/Right, y для Top/Bottom).
    origin: f64,
    /// Размер ленты по разделяющей оси.
    size: f64,
    /// Число столбцов в ленте.
    cols: usize,
    /// Число рядов в ленте.
    rows: usize,
    /// Ширина миниатюры.
    thumb_w: f64,
    /// Высота миниатюры.
    thumb_h: f64,
}

/// PIN: крупный участник с одной стороны и лента миниатюр 16:9 с другой.
/// Размер ленты подбирается перебором числа столбцов/рядов (логика библиотеки),
/// крупный участник занимает остаток за вычетом зазора.
fn pin(count: usize, side: PinSide, width: f64, height: f64, gap: f64, margin: f64) -> Vec<SlotRect> {
    let count = count.max(1);
    let inner_w = (width - 2.0 * margin).max(1.0);
    let inner_h = (height - 2.0 * margin).max(1.0);
    let gap = clamp_gap(gap, count, inner_w, inner_h);
    if count == 1 {
        return vec![SlotRect::new(margin, margin, inner_w, inner_h)];
    }
    let others = count - 1;
    let vertical = matches!(side, PinSide::Left | PinSide::Right);
    // Разделяющая ось: длина ленты и расстояние от края поля до её начала.
    let strip = if vertical {
        let strip = best_strip(others, inner_h, gap);
        let size = strip.size;
        let origin = match side {
            PinSide::Left => margin + inner_w - size,
            _ => margin,
        };
        let (cols, rows) = (strip.cols, strip.rows);
        let (thumb_w, thumb_h) = (strip.thumb_w, strip.thumb_h);
        StripLayout { origin, size, cols, rows, thumb_w, thumb_h }
    } else {
        let strip = best_strip_horizontal(others, inner_w, inner_h, gap);
        let size = strip.size;
        let origin = match side {
            PinSide::Top => margin + inner_h - size,
            _ => margin,
        };
        let (cols, rows) = (strip.cols, strip.rows);
        let (thumb_w, thumb_h) = (strip.thumb_w, strip.thumb_h);
        StripLayout { origin, size, cols, rows, thumb_w, thumb_h }
    };
    let mut rects = Vec::with_capacity(count);
    if vertical {
        let main_width = (inner_w - strip.size - gap).max(1.0);
        match side {
            PinSide::Left => rects.push(SlotRect::new(margin, margin, main_width, inner_h)),
            _ => rects.push(SlotRect::new(margin + strip.size + gap, margin, main_width, inner_h)),
        }
    } else {
        let main_height = (inner_h - strip.size - gap).max(1.0);
        match side {
            PinSide::Top => rects.push(SlotRect::new(margin, margin, inner_w, main_height)),
            _ => rects.push(SlotRect::new(margin, margin + strip.size + gap, inner_w, main_height)),
        }
    }
    // Лента: сетка миниатюр 16:9, центрированная в своей зоне; целочисленный
    // разрез по краям, чтобы соседние миниатюры не наезжали друг на друга.
    let grid_width = strip.cols as f64 * strip.thumb_w + strip.cols.saturating_sub(1) as f64 * gap;
    let grid_height = strip.rows as f64 * strip.thumb_h + strip.rows.saturating_sub(1) as f64 * gap;
    let row_edges: Vec<f64> = if vertical {
        let grid_top = margin + (inner_h - grid_height) / 2.0;
        (0..=strip.rows).map(|row| (grid_top + row as f64 * (strip.thumb_h + gap)).round()).collect()
    } else {
        let grid_top = strip.origin + (strip.size - grid_height) / 2.0;
        (0..=strip.rows).map(|row| (grid_top + row as f64 * (strip.thumb_h + gap)).round()).collect()
    };
    let grid_left =
        if vertical { strip.origin + (strip.size - grid_width) / 2.0 } else { margin + (inner_w - grid_width) / 2.0 };
    for row in 0..strip.rows {
        let in_row = (others - row * strip.cols).min(strip.cols);
        let row_width = in_row as f64 * strip.thumb_w + in_row.saturating_sub(1) as f64 * gap;
        let row_left = grid_left + (grid_width - row_width) / 2.0;
        let edges = spaced_edges(in_row, row_left, strip.thumb_w, gap);
        let y = row_edges[row];
        let h = (row_edges[row + 1] - y - gap).max(1.0);
        for col in 0..in_row {
            let x = edges[col];
            let w = (edges[col + 1] - x - gap).max(1.0);
            rects.push(SlotRect::new(x, y, w, h));
        }
    }
    rects
}

/// Слишком большой зазор (например `vks_pin_l_g32_64`) даёт отрицательные
/// ячейки/выход за холст: тогда зазор обнуляется, чтобы геометрия оставалась
/// валидной (id при этом не меняется — это защита рендера, не парсинг).
fn clamp_gap(gap: f64, count: usize, inner_w: f64, inner_h: f64) -> f64 {
    if !gap.is_finite() || gap <= 0.0 {
        return 0.0;
    }
    let slots_axis = (count.max(1)) as f64;
    let smallest = inner_w.min(inner_h);
    if smallest - (slots_axis - 1.0) * gap < slots_axis {
        0.0
    } else {
        gap
    }
}

/// Границы `count` ячеек по одной оси с зазором `gap` (целочисленный разрез).
fn spaced_edges(count: usize, start: f64, cell: f64, gap: f64) -> Vec<f64> {
    let width = count as f64 * cell + count.saturating_sub(1) as f64 * gap;
    let mut edges: Vec<f64> = (0..count).map(|index| (start + index as f64 * (cell + gap)).round()).collect();
    edges.push((start + width).round());
    edges
}

/// Подбор вертикальной ленты (справа/слева): миниатюры 16:9, зона 10–40%
/// ширины холста (порт библиотеки).
fn best_strip(others: usize, inner_h: f64, gap: f64) -> StripLayout {
    let mut best = StripLayout { origin: 0.0, size: 0.0, cols: 1, rows: others, thumb_w: 0.0, thumb_h: 0.0 };
    let mut best_score = 0.0_f64;
    for cols in 1..=others.min(3) {
        let rows = others.div_ceil(cols);
        let thumb_h = (inner_h - (rows - 1) as f64 * gap) / rows as f64;
        let thumb_w = thumb_h * 16.0 / 9.0;
        let required = cols as f64 * thumb_w + (cols - 1) as f64 * gap;
        // Зона ленты измеряется относительно высоты? Нет: относительно ширины
        // холста библиотека не знает; берём долю от высоты, чтобы 1–3 столбца
        // давали предсказуемую полосу 10–40% ширины при 16:9.
        let area_ratio = required / (inner_h * 16.0 / 9.0).max(1.0);
        let score = thumb_w * thumb_h * (1.0 + (1.0 - area_ratio) * 0.5);
        if (0.1..=0.4).contains(&area_ratio) && score > best_score {
            best = StripLayout { origin: 0.0, size: required, cols, rows, thumb_w, thumb_h };
            best_score = score;
        }
    }
    if best_score == 0.0 {
        let size = (inner_h * 0.2).clamp(inner_h * 0.12, inner_h * 0.35);
        let thumb_w = size;
        let thumb_h = (thumb_w * 9.0 / 16.0).min(inner_h / others.max(1) as f64);
        best = StripLayout { origin: 0.0, size, cols: 1, rows: others, thumb_w, thumb_h };
    }
    best
}

/// Подбор горизонтальной ленты (сверху/снизу): миниатюры 16:9, зона 12–45%
/// высоты холста, высота миниатюры не меньше 40 px (порт библиотеки).
fn best_strip_horizontal(others: usize, inner_w: f64, inner_h: f64, gap: f64) -> StripLayout {
    let mut best = StripLayout { origin: 0.0, size: 0.0, cols: others, rows: 1, thumb_w: 0.0, thumb_h: 0.0 };
    let mut best_area = 0.0_f64;
    for rows in 1..=others {
        let cols = others.div_ceil(rows);
        let thumb_w = (inner_w - (cols - 1) as f64 * gap) / cols as f64;
        let thumb_h = thumb_w * 9.0 / 16.0;
        let required = rows as f64 * thumb_h + (rows - 1) as f64 * gap;
        let area_ratio = required / inner_h;
        // Порог библиотеки 40 px задан для 720p: на превью 320×180 и холсте
        // 1080p он масштабируется, иначе иконка и композит выбирают разные сетки.
        if area_ratio > 0.45 || thumb_h < 40.0 * (inner_h / 720.0) {
            continue;
        }
        let area = thumb_w * thumb_h;
        if area > best_area {
            best = StripLayout { origin: 0.0, size: required, cols, rows, thumb_w, thumb_h };
            best_area = area;
        }
    }
    if best_area == 0.0 {
        let size = (inner_h * 0.2).clamp(inner_h * 0.12, inner_h * 0.45);
        let thumb_h = size / others.max(1) as f64;
        let thumb_w = (thumb_h * 16.0 / 9.0).min(inner_w);
        best = StripLayout { origin: 0.0, size, cols: 1, rows: others, thumb_w, thumb_h };
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_понимает_оба_семейства() {
        assert_eq!(parse("vks_grid_9"), Some(GridScheme { count: 9, pin: None, options: GridOptions::default() }));
        assert_eq!(
            parse("vks_pin_r_8"),
            Some(GridScheme { count: 8, pin: Some(PinSide::Right), options: GridOptions::default() })
        );
        assert_eq!(
            parse("vks_grid_g12_m24_25"),
            Some(GridScheme { count: 25, pin: None, options: GridOptions { gap: 12, margin: 24 } })
        );
        assert_eq!(
            parse("vks_pin_l_g8_7"),
            Some(GridScheme { count: 7, pin: Some(PinSide::Left), options: GridOptions { gap: 8, margin: 0 } })
        );
        assert_eq!(parse("vks_grid_9_(2)"), Some(GridScheme { count: 9, pin: None, options: GridOptions::default() }));
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
            for pin in [None, Some(PinSide::Left), Some(PinSide::Right), Some(PinSide::Top), Some(PinSide::Bottom)] {
                for options in [
                    GridOptions::default(),
                    GridOptions { gap: 8, margin: 0 },
                    GridOptions { gap: 0, margin: 16 },
                    GridOptions { gap: 12, margin: 24 },
                ] {
                    let scheme = GridScheme { count, pin, options };
                    assert_eq!(parse(&scheme.id()), Some(scheme));
                }
            }
        }
    }

    #[test]
    fn slot_count_совпадает_с_числом_окон() {
        assert_eq!(slot_count("vks_grid_g8_m16_5"), Some(5));
        assert_eq!(slot_count("codian_01"), None);
    }
}
