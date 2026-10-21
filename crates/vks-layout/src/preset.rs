//! Пресеты раскладок TrueConf: идентификаторы, схемы и геометрия слотов.
//!
//! Вендорный фронтенд PNM адресует раскладки строковыми id вида
//! `preset_selected_b_4_(2)` / `preset_equal_25` / `preset_selected_rb_overlap`.
//! Этот модуль — единственный источник правды о том, как такой id разобрать
//! и какую геометрию он означает:
//!
//! - [`PresetId::parse`] — разбор id (схема + размер + суффиксы);
//! - [`geometry`] — пиксельные [`SlotRect`] на канвасе;
//! - [`catalog`] — каталог пресетов для `GET /api/v3/video_layout_grids`;
//! - [`builtin_grids`] — фиксированные (custom) геометрии редактора.
//!
//! Схемы (`PresetScheme`): `equal` (равномерная сетка), `selected`
//! (выделенный участник + полоса/угол/pip), `double` (две крупные зоны +
//! полоса), `default` (вендорная «общая» раскладка).
//!
//! Инварианты геометрии: все rect внутри canvas, значения конечные
//! (никаких NaN), пересечения допустимы только у `overlap`/`pip`-схем.
//! Модуль dep-light: только serde.

use serde::{Deserialize, Serialize};

/// Позиция акцентной области/полосы в пресете.
///
/// `B`/`T`/`R`/`L` — крупная зона внизу/вверху/справа/слева, `Rb`/`Rt` —
/// pip-угол (правый нижний/верхний), `C` — центр с кольцом, `Rlr` — две
/// боковые зоны, `Btb` — верхняя и нижняя полосы.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Position {
    /// Крупная зона снизу.
    B,
    /// Крупная зона сверху.
    T,
    /// Крупная зона справа.
    R,
    /// Крупная зона слева.
    L,
    /// Pip-угол: правый нижний.
    Rb,
    /// Pip-угол: правый верхний.
    Rt,
    /// Крупная зона в центре + кольцо мелких.
    C,
    /// Две боковые зоны.
    Rlr,
    /// Верхняя и нижняя полосы.
    Btb,
}

impl Position {
    /// Канонический токен id (`b`, `t`, …, `btb`).
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::B => "b",
            Self::T => "t",
            Self::R => "r",
            Self::L => "l",
            Self::Rb => "rb",
            Self::Rt => "rt",
            Self::C => "c",
            Self::Rlr => "rlr",
            Self::Btb => "btb",
        }
    }

    /// Разбирает токен позиции из `preset_*` id.
    pub fn from_token(token: &str) -> Option<Self> {
        Some(match token {
            "b" => Self::B,
            "t" => Self::T,
            "r" => Self::R,
            "l" => Self::L,
            "rb" => Self::Rb,
            "rt" => Self::Rt,
            "c" => Self::C,
            "rlr" => Self::Rlr,
            "btb" => Self::Btb,
            _ => return None,
        })
    }

    /// True для pip-углов (`rb`/`rt`) — раскладка «большая зона + окошко».
    pub const fn is_corner(self) -> bool {
        matches!(self, Self::Rb | Self::Rt)
    }
}

/// Схема раскладки.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PresetScheme {
    /// Равномерная сбалансированная сетка (последний ряд по центру).
    Equal,
    /// Выделенный участник + полоса/угол/кольцо мелких.
    Selected(Position),
    /// Две крупные зоны + полоса мелких.
    Double(Position),
    /// Вендорная «общая» раскладка (`preset_default_13`).
    Default,
}

impl PresetScheme {
    /// Канонический префикс схемы (без `preset_`).
    pub const fn family(self) -> &'static str {
        match self {
            Self::Equal => "equal",
            Self::Selected(_) => "selected",
            Self::Double(_) => "double",
            Self::Default => "default",
        }
    }
}

/// Разобранный идентификатор пресета.
///
/// `overlap`/`pip` — вендорный суффикс `_overlap`: маленькие окна лежат
/// поверх крупной зоны (пересечение допустимо). `variant` — суффикс `_(N)`,
/// меняющий пропорцию/pip-сторону.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PresetId {
    /// Схема раскладки.
    pub scheme: PresetScheme,
    /// Число окон в раскладке.
    pub size: u8,
    /// Номер варианта (`_(2)`), если задан.
    pub variant: Option<u8>,
    /// Признак `_overlap` (маленькие окна поверх большой зоны).
    pub overlap: bool,
    /// PiP-режим (синоним overlap на уровне геометрии).
    pub pip: bool,
}

/// Собирает полный id раскладки из базовой схемы и числа окон.
///
/// PNM UI присылает `payload.preset = "preset_equal"` и отдельно `size = 4`
/// (см. `Th`/`Nee` в бандле). Хранить и отдавать нужно ровно как у эталона —
/// базу и размер, — а полный id собирать только в потребителях (геометрия,
/// calculate, MCU). Иначе редактор не подсвечивает выбранную схему, а
/// `PresetId::parse` без суффикса откатывается к размеру 13.
pub fn resolve(preset: &str, size: Option<u8>) -> String {
    let preset = preset.trim();
    if preset.is_empty() {
        return String::new();
    }
    if has_explicit_size(preset) {
        return preset.to_string();
    }
    match size.filter(|size| *size > 0) {
        Some(size) => format!("{preset}_{size}"),
        None => preset.to_string(),
    }
}

/// True, если в id уже есть числовой размер (`preset_equal_4`,
/// `preset_selected_b_4_(2)`, `preset_selected_rb_overlap_2`).
pub fn has_explicit_size(preset: &str) -> bool {
    let without_variant = preset.rsplit_once("_(").map_or(preset, |(base, _)| base);
    without_variant.rsplit('_').next().is_some_and(|tail| tail.parse::<u8>().is_ok())
}

impl PresetId {
    /// Размер по умолчанию для вендорных id без числа (`preset_equal`,
    /// `preset_selected_rb_overlap`): вендорная «общая» раскладка — 13 окон.
    pub const DEFAULT_SIZE: u8 = 13;

    /// Создаёт равномерный пресет заданного размера.
    pub fn equal(size: u8) -> Self {
        Self { scheme: PresetScheme::Equal, size, variant: None, overlap: false, pip: false }
    }

    /// Разбирает вендорный id раскладки.
    ///
    /// Поддерживаются формы `preset_equal_25`, `preset_selected_b_4_(2)`,
    /// `preset_selected_rb_overlap`, `preset_selected_b_overlap_4`,
    /// `preset_double_rlr_10`, `preset_default_13`; `square_N` трактуется как
    /// равномерная сетка. Неизвестная схема/позиция/мусорные токены → `None`.
    pub fn parse(input: &str) -> Option<Self> {
        let rest = input.trim().strip_prefix("preset_")?;
        let mut tokens: Vec<&str> = rest.split('_').filter(|token| !token.is_empty()).collect();
        if tokens.is_empty() {
            return None;
        }

        // `_(N)` — всегда последний суффикс.
        let mut variant = None;
        if let Some(last) = tokens.last().copied() {
            if let Some(inner) = last.strip_prefix('(').and_then(|token| token.strip_suffix(')')) {
                let value: u8 = inner.parse().ok()?;
                if value == 0 {
                    return None;
                }
                variant = Some(value);
                tokens.pop();
            }
        }

        // `_overlap` может стоять до или после числа (`preset_selected_b_overlap_4`,
        // `preset_selected_rb_overlap_2`).
        let mut overlap = false;
        if let Some(index) = tokens.iter().position(|token| *token == "overlap") {
            tokens.remove(index);
            overlap = true;
        }

        let head = *tokens.first()?;
        let scheme = match head {
            "default" => PresetScheme::Default,
            "equal" | "square" => PresetScheme::Equal,
            "selected" => {
                let position = tokens.get(1).and_then(|token| Position::from_token(token))?;
                PresetScheme::Selected(position)
            }
            "double" => {
                let position = tokens.get(1).and_then(|token| Position::from_token(token))?;
                PresetScheme::Double(position)
            }
            _ => return None,
        };

        let expected_tokens = if matches!(scheme, PresetScheme::Selected(_) | PresetScheme::Double(_)) { 2 } else { 1 };
        let size_token = tokens.get(expected_tokens).copied();
        if tokens.len() > expected_tokens + usize::from(size_token.is_some()) {
            return None;
        }
        let size = match size_token {
            Some(token) => token.parse::<u8>().ok().filter(|value| *value > 0)?,
            None => Self::DEFAULT_SIZE,
        };

        Some(Self { scheme, size, variant, overlap, pip: overlap })
    }
}

impl std::fmt::Display for PresetId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let base = match self.scheme {
            PresetScheme::Equal => "equal".to_string(),
            PresetScheme::Default => "default".to_string(),
            PresetScheme::Selected(position) => format!("selected_{}", position.as_str()),
            PresetScheme::Double(position) => format!("double_{}", position.as_str()),
        };
        formatter.write_str("preset_")?;
        formatter.write_str(&base)?;
        if self.overlap {
            formatter.write_str("_overlap")?;
        }
        write!(formatter, "_{}", self.size)?;
        if let Some(variant) = self.variant {
            write!(formatter, "_({variant})")?;
        }
        Ok(())
    }
}

/// Прямоугольник слота в пикселях канваса.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SlotRect {
    /// Левый край.
    pub x: f64,
    /// Верхний край.
    pub y: f64,
    /// Ширина.
    pub width: f64,
    /// Высота.
    pub height: f64,
}

impl SlotRect {
    /// Создаёт rect слота.
    pub const fn new(x: f64, y: f64, width: f64, height: f64) -> Self {
        Self { x, y, width, height }
    }

    /// True, если все координаты конечны и размер неотрицателен.
    pub fn is_valid(&self) -> bool {
        self.x.is_finite()
            && self.y.is_finite()
            && self.width.is_finite()
            && self.height.is_finite()
            && self.x >= 0.0
            && self.y >= 0.0
            && self.width >= 0.0
            && self.height >= 0.0
    }
}

/// Нормализованный rect `[0.0, 1.0]` на канвасе.
fn unit(x: f64, y: f64, width: f64, height: f64) -> SlotRect {
    let x = x.clamp(0.0, 1.0);
    let y = y.clamp(0.0, 1.0);
    let width = width.clamp(0.0, 1.0 - x);
    let height = height.clamp(0.0, 1.0 - y);
    SlotRect::new(x, y, width, height)
}

/// Зеркалит нормализованный rect по горизонтали (вариант `_(2)` у pip-схем).
fn mirror_x(rect: SlotRect) -> SlotRect {
    unit(1.0 - rect.x - rect.width, rect.y, rect.width, rect.height)
}

/// Санитизация размера канваса: нечисловой/нулевой → fallback.
fn dimension(value: f64, fallback: f64) -> f64 {
    if value.is_finite() && value > 0.0 {
        value
    } else {
        fallback
    }
}

/// Балансированная сетка для `count` окон: минимизирует `|rows - cols|`.
fn balanced_grid(count: usize) -> (usize, usize) {
    if count <= 1 {
        return (1, 1);
    }
    let mut best = (count, 1usize);
    let mut best_score = f64::INFINITY;
    for rows in 1..=count {
        let cols = count.div_ceil(rows);
        // На равном искажении предпочитаем меньше строк (шире раскладка).
        let score = (rows as f64 - cols as f64).abs() / cols as f64 + rows as f64 * 1e-9;
        if score < best_score {
            best_score = score;
            best = (rows, cols);
        }
    }
    best
}

/// Раскладывает `count` окон внутри региона `region` сбалансированной сеткой,
/// центрируя неполный последний ряд.
fn fill_region(region: SlotRect, count: usize) -> Vec<SlotRect> {
    if count == 0 {
        return Vec::new();
    }
    let (rows, cols) = balanced_grid(count);
    let cell_width = region.width / cols as f64;
    let cell_height = region.height / rows as f64;
    (0..count)
        .map(|index| {
            let row = index / cols;
            let col = index % cols;
            let in_row = cols.min(count - row * cols);
            let row_offset = (region.width - in_row as f64 * cell_width) / 2.0;
            unit(
                region.x + row_offset + col as f64 * cell_width,
                region.y + row as f64 * cell_height,
                cell_width,
                cell_height,
            )
        })
        .collect()
}

/// Равномерная сетка; неполный последний ряд по центру (variant > 1 — вправо).
fn equal_slots(count: usize, variant: Option<u8>) -> Vec<SlotRect> {
    if count == 0 {
        return Vec::new();
    }
    let (rows, cols) = balanced_grid(count);
    let cell_width = 1.0 / cols as f64;
    let cell_height = 1.0 / rows as f64;
    (0..count)
        .map(|index| {
            let row = index / cols;
            let col = index % cols;
            let in_row = cols.min(count - row * cols);
            let leftover = 1.0 - in_row as f64 * cell_width;
            let row_offset = match variant {
                Some(value) if value > 1 => leftover,
                _ => leftover / 2.0,
            };
            unit(row_offset + col as f64 * cell_width, row as f64 * cell_height, cell_width, cell_height)
        })
        .collect()
}

/// Схема `selected`: одна крупная зона + полоса/угол/кольцо мелких.
fn selected_slots(position: Position, count: usize, variant: Option<u8>) -> Vec<SlotRect> {
    if count <= 1 {
        return vec![unit(0.0, 0.0, 1.0, 1.0)];
    }
    let smalls = count - 1;
    let strip = if variant.is_some() { 0.34 } else { 0.25 };
    let mut slots = match position {
        Position::B => {
            // Крупная зона снизу, полоса мелких сверху.
            let mut slots = vec![unit(0.0, strip, 1.0, 1.0 - strip)];
            slots.extend(fill_region(unit(0.0, 0.0, 1.0, strip), smalls));
            slots
        }
        Position::T => {
            // Крупная зона сверху, полоса мелких снизу.
            let mut slots = vec![unit(0.0, 0.0, 1.0, 1.0 - strip)];
            slots.extend(fill_region(unit(0.0, 1.0 - strip, 1.0, strip), smalls));
            slots
        }
        Position::L => {
            let mut slots = vec![unit(0.0, 0.0, 1.0 - strip, 1.0)];
            slots.extend(fill_region(unit(1.0 - strip, 0.0, strip, 1.0), smalls));
            slots
        }
        Position::R => {
            let mut slots = vec![unit(strip, 0.0, 1.0 - strip, 1.0)];
            slots.extend(fill_region(unit(0.0, 0.0, strip, 1.0), smalls));
            slots
        }
        Position::Rb | Position::Rt => corner_slots(position, strip, smalls, variant.is_some()),
        Position::C => center_slots(variant, smalls),
        Position::Rlr => two_sides_slots(variant, count),
        Position::Btb => two_bands_slots(variant, count),
    };
    slots.truncate(count);
    slots
}

/// `selected_rb`/`selected_rt`: крупная зона + pip-угол (вариант `_(2)`
/// переносит pip на противоположную сторону).
fn corner_slots(corner: Position, strip: f64, smalls: usize, flip: bool) -> Vec<SlotRect> {
    let (big, first_region, second_region, pip_cell) = match corner {
        Position::Rt => (
            unit(0.0, strip, 1.0 - strip, 1.0 - strip),
            unit(1.0 - strip, strip, strip, 1.0 - strip),
            unit(0.0, 0.0, 1.0 - strip, strip),
            unit(1.0 - strip, 0.0, strip, strip),
        ),
        // Position::Rb и любой fallback — pip в правом нижнем углу.
        _ => (
            unit(0.0, 0.0, 1.0 - strip, 1.0 - strip),
            unit(1.0 - strip, 0.0, strip, 1.0 - strip),
            unit(0.0, 1.0 - strip, 1.0 - strip, strip),
            unit(1.0 - strip, 1.0 - strip, strip, strip),
        ),
    };
    let (big, first_region, second_region, pip_cell) = if flip {
        (mirror_x(big), mirror_x(first_region), mirror_x(second_region), mirror_x(pip_cell))
    } else {
        (big, first_region, second_region, pip_cell)
    };
    let mut slots = vec![big];
    if smalls == 0 {
        return slots;
    }
    if smalls == 1 {
        slots.push(pip_cell);
        return slots;
    }
    let first_count = smalls.div_ceil(2);
    slots.extend(fill_region(first_region, first_count));
    slots.extend(fill_region(second_region, smalls - first_count));
    slots
}

/// `selected_c`: крупная зона в центре + кольцо мелких по периметру.
fn center_slots(variant: Option<u8>, smalls: usize) -> Vec<SlotRect> {
    let ring = if variant.is_some() { 0.16 } else { 0.2 };
    let big = unit(ring, ring, 1.0 - 2.0 * ring, 1.0 - 2.0 * ring);
    let top = unit(0.0, 0.0, 1.0, ring);
    let bottom = unit(0.0, 1.0 - ring, 1.0, ring);
    let left = unit(0.0, ring, ring, 1.0 - 2.0 * ring);
    let right = unit(1.0 - ring, ring, ring, 1.0 - 2.0 * ring);
    let top_count = smalls.div_ceil(4);
    let bottom_count = smalls.saturating_sub(top_count).div_ceil(3).min(smalls - top_count);
    let left_count = (smalls - top_count - bottom_count).div_ceil(2);
    let right_count = smalls - top_count - bottom_count - left_count;
    let mut slots = vec![big];
    slots.extend(fill_region(top, top_count));
    slots.extend(fill_region(bottom, bottom_count));
    slots.extend(fill_region(left, left_count));
    slots.extend(fill_region(right, right_count));
    slots
}

/// `selected_rlr`: две крупные боковые зоны + полоса мелких между ними.
fn two_sides_slots(variant: Option<u8>, count: usize) -> Vec<SlotRect> {
    if count <= 1 {
        return vec![unit(0.0, 0.0, 1.0, 1.0)];
    }
    if count == 2 {
        return vec![unit(0.0, 0.0, 0.5, 1.0), unit(0.5, 0.0, 0.5, 1.0)];
    }
    let strip = if variant.is_some() { 0.34 } else { 0.28 };
    let big_width = (1.0 - strip) / 2.0;
    let mut slots = vec![unit(0.0, 0.0, big_width, 1.0), unit(1.0 - big_width, 0.0, big_width, 1.0)];
    slots.extend(fill_region(unit(big_width, 0.0, strip, 1.0), count - 2));
    slots
}

/// `selected_btb`: крупная зона в середине + верхняя и нижняя полосы мелких.
fn two_bands_slots(variant: Option<u8>, count: usize) -> Vec<SlotRect> {
    if count <= 1 {
        return vec![unit(0.0, 0.0, 1.0, 1.0)];
    }
    let strip = if variant.is_some() { 0.3 } else { 0.24 };
    let smalls = count - 1;
    if smalls == 1 {
        // Одна мелкая полоса сверху, крупная зона занимает остальное.
        return vec![unit(0.0, strip, 1.0, 1.0 - strip), unit(0.0, 0.0, 1.0, strip)];
    }
    let top_count = smalls.div_ceil(2);
    let mut slots = vec![unit(0.0, strip, 1.0, 1.0 - 2.0 * strip)];
    slots.extend(fill_region(unit(0.0, 0.0, 1.0, strip), top_count));
    slots.extend(fill_region(unit(0.0, 1.0 - strip, 1.0, strip), smalls - top_count));
    slots
}

/// Схема `double`: две крупные зоны + полоса мелких.
fn double_slots(position: Position, count: usize, variant: Option<u8>) -> Vec<SlotRect> {
    if count <= 1 {
        return vec![unit(0.0, 0.0, 1.0, 1.0)];
    }
    if count == 2 {
        return vec![unit(0.0, 0.0, 0.5, 1.0), unit(0.5, 0.0, 0.5, 1.0)];
    }
    match position {
        Position::B => {
            // Две крупные зоны снизу, полоса мелких сверху.
            let strip = if variant.is_some() { 0.4 } else { 1.0 / 3.0 };
            let mut slots = vec![unit(0.0, strip, 0.5, 1.0 - strip), unit(0.5, strip, 0.5, 1.0 - strip)];
            slots.extend(fill_region(unit(0.0, 0.0, 1.0, strip), count - 2));
            slots
        }
        Position::T => {
            // Две крупные зоны сверху, полоса мелких снизу.
            let strip = if variant.is_some() { 0.4 } else { 1.0 / 3.0 };
            let mut slots = vec![unit(0.0, 0.0, 0.5, 1.0 - strip), unit(0.5, 0.0, 0.5, 1.0 - strip)];
            slots.extend(fill_region(unit(0.0, 1.0 - strip, 1.0, strip), count - 2));
            slots
        }
        // `double_rlr` и любой fallback: две крупные колонки + центральная полоса.
        _ => {
            let strip = if variant.is_some() { 0.34 } else { 0.28 };
            let big_width = (1.0 - strip) / 2.0;
            let mut slots = vec![unit(0.0, 0.0, big_width, 1.0), unit(1.0 - big_width, 0.0, big_width, 1.0)];
            slots.extend(fill_region(unit(big_width, 0.0, strip, 1.0), count - 2));
            slots
        }
    }
}

/// `overlap`/`pip`: крупная зона на весь канвас + маленькие окна поверх.
fn overlay_slots(scheme: &PresetScheme, count: usize, variant: Option<u8>, overlap_factor: f64) -> Vec<SlotRect> {
    let big = unit(0.0, 0.0, 1.0, 1.0);
    if count <= 1 {
        return vec![big];
    }
    let smalls = count - 1;
    let factor = if overlap_factor.is_finite() && overlap_factor > 0.0 { overlap_factor.clamp(0.25, 2.5) } else { 1.0 };
    let position = match scheme {
        PresetScheme::Selected(position) | PresetScheme::Double(position) => *position,
        PresetScheme::Equal | PresetScheme::Default => Position::Rb,
    };
    let cols = (smalls as f64).sqrt().ceil() as usize;
    let rows = smalls.div_ceil(cols);
    let max_cell_width = 0.85 / cols as f64;
    let max_cell_height = 0.85 / rows as f64;
    let mut cell = (0.34 * factor).clamp(0.08, 0.6);
    cell = cell.min(max_cell_width).min(max_cell_height);
    let box_width = cell * cols as f64;
    let box_height = cell * rows as f64;
    let mut box_x = match position {
        Position::B | Position::T | Position::C => (1.0 - box_width) / 2.0,
        Position::R | Position::Rb | Position::Rt | Position::Rlr | Position::Btb => 1.0 - box_width,
        Position::L => 0.0,
    };
    let box_y = match position {
        Position::T | Position::Rt => 0.0,
        Position::B | Position::Rlr | Position::Btb => 1.0 - box_height,
        Position::R | Position::L | Position::C => (1.0 - box_height) / 2.0,
        Position::Rb => 1.0 - box_height,
    };
    if variant.is_some() && position.is_corner() {
        // `_(2)` у pip-схем переносит окошко на противоположную сторону.
        box_x = 1.0 - box_width - box_x;
    }
    let mut slots = vec![big];
    slots.extend((0..smalls).map(|index| {
        let col = index % cols;
        let row = index / cols;
        unit(box_x + col as f64 * cell, box_y + row as f64 * cell, cell, cell)
    }));
    slots
}

/// Геометрия пресета в пикселях канваса `width` × `height`.
///
/// `overlap_factor` масштабирует окна pip-режима (значения ≤ 0/NaN → 1.0);
/// `pip = true` принудительно включает наложение даже у не-overlap пресета.
/// Возвращает ровно `preset.size` слотов (минимум один) — все внутри канваса,
/// без NaN/бесконечностей. Пересечения возможны только у overlap/pip.
pub fn geometry(preset: &PresetId, width: f64, height: f64, overlap_factor: f64, pip: bool) -> Vec<SlotRect> {
    let width = dimension(width, 1280.0);
    let height = dimension(height, 720.0);
    let count = usize::from(preset.size.max(1));
    let overlay = preset.overlap || preset.pip || pip;
    let normalized = if overlay {
        overlay_slots(&preset.scheme, count, preset.variant, overlap_factor)
    } else {
        match preset.scheme {
            PresetScheme::Equal => equal_slots(count, preset.variant),
            PresetScheme::Selected(position) => selected_slots(position, count, preset.variant),
            PresetScheme::Double(position) => double_slots(position, count, preset.variant),
            PresetScheme::Default => equal_slots(count, preset.variant),
        }
    };
    normalized.into_iter().map(|rect| scale_rect(rect, width, height)).collect()
}

/// Масштабирует нормализованный rect в пиксели и прижимает к канвасу.
fn scale_rect(rect: SlotRect, width: f64, height: f64) -> SlotRect {
    let x = (rect.x * width).clamp(0.0, width);
    let y = (rect.y * height).clamp(0.0, height);
    let rect_width = (rect.width * width).clamp(0.0, width - x);
    let rect_height = (rect.height * height).clamp(0.0, height - y);
    SlotRect::new(x, y, rect_width, rect_height)
}

/// Элемент каталога пресетов.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PresetInfo {
    /// Вендорный id (`preset_selected_b_4`).
    pub id: String,
    /// Схема раскладки.
    pub scheme: PresetScheme,
    /// Число окон.
    pub size: u8,
    /// Признак `_overlap`.
    pub overlap: bool,
    /// PiP-режим.
    pub pip: bool,
    /// Человекочитаемое имя для списков/поиска.
    pub display_name: String,
}

/// Раскладки, для которых каталог гарантирует пресеты.
const SELECTED_POSITIONS: [Position; 9] = [
    Position::B,
    Position::T,
    Position::R,
    Position::L,
    Position::Rb,
    Position::Rt,
    Position::C,
    Position::Rlr,
    Position::Btb,
];
/// Размеры схемы `selected` (включая вендорный «общий» размер 13).
const SELECTED_SIZES: [u8; 8] = [2, 3, 4, 6, 9, 12, 16, 25];
/// Позиции, для которых вендор определяет `_overlap` (pip) варианты.
const OVERLAP_POSITIONS: [Position; 6] =
    [Position::B, Position::T, Position::R, Position::L, Position::Rb, Position::Rt];
/// Схемы с вендорским суффиксом `_(2)`.
const VARIANT_POSITIONS: [Position; 2] = [Position::B, Position::T];
/// Размеры вариантов `_(2)`.
const VARIANT_SIZES: [u8; 6] = [4, 6, 9, 12, 16, 25];
/// Позиции схемы `double`.
const DOUBLE_POSITIONS: [Position; 3] = [Position::B, Position::T, Position::Rlr];
/// Размеры схемы `double`.
const DOUBLE_SIZES: [u8; 5] = [2, 4, 6, 9, 12];

/// Человекочитаемое имя позиции для `display_name`.
fn position_display(position: Position) -> &'static str {
    match position {
        Position::B => "снизу",
        Position::T => "сверху",
        Position::R => "справа",
        Position::L => "слева",
        Position::Rb => "правый нижний",
        Position::Rt => "правый верхний",
        Position::C => "центр",
        Position::Rlr => "боковые",
        Position::Btb => "верх/низ",
    }
}

/// Имя пресета для списков PNM.
fn preset_display_name(preset: &PresetId) -> String {
    let mut name = match preset.scheme {
        PresetScheme::Equal => format!("Сетка {}", preset.size),
        PresetScheme::Default => format!("По умолчанию ({})", preset.size),
        PresetScheme::Selected(position) => {
            format!("Выделенный ({}) {}", position_display(position), preset.size)
        }
        PresetScheme::Double(position) => format!("Двойной ({}) {}", position_display(position), preset.size),
    };
    if preset.overlap {
        name.push_str(" (перекрытие)");
    }
    if let Some(variant) = preset.variant {
        name.push_str(&format!(" ({variant})"));
    }
    name
}

fn preset_info(preset: PresetId) -> PresetInfo {
    PresetInfo {
        id: preset.to_string(),
        scheme: preset.scheme,
        size: preset.size,
        overlap: preset.overlap,
        pip: preset.pip,
        display_name: preset_display_name(&preset),
    }
}

/// Каталог вендорных пресетов для `/api/v3/video_layout_grids`.
///
/// Включает `preset_default_13`, `preset_equal_1..=25`, все позиции
/// `selected` для размеров [`SELECTED_SIZES`], pip-варианты `_overlap`,
/// варианты `_(2)` и схемы `double`. Порядок детерминирован: default →
/// equal → selected → overlap → variants → double.
pub fn catalog() -> Vec<PresetInfo> {
    let mut items = Vec::new();

    items.push(preset_info(PresetId {
        scheme: PresetScheme::Default,
        size: 13,
        variant: None,
        overlap: false,
        pip: false,
    }));

    for size in 1..=25u8 {
        items.push(preset_info(PresetId::equal(size)));
    }

    for position in SELECTED_POSITIONS {
        for size in SELECTED_SIZES {
            items.push(preset_info(PresetId {
                scheme: PresetScheme::Selected(position),
                size,
                variant: None,
                overlap: false,
                pip: false,
            }));
        }
    }

    for position in OVERLAP_POSITIONS {
        for size in SELECTED_SIZES {
            items.push(preset_info(PresetId {
                scheme: PresetScheme::Selected(position),
                size,
                variant: None,
                overlap: true,
                pip: true,
            }));
        }
    }

    for position in VARIANT_POSITIONS {
        for size in VARIANT_SIZES {
            items.push(preset_info(PresetId {
                scheme: PresetScheme::Selected(position),
                size,
                variant: Some(2),
                overlap: false,
                pip: false,
            }));
        }
    }

    for position in DOUBLE_POSITIONS {
        for size in DOUBLE_SIZES {
            items.push(preset_info(PresetId {
                scheme: PresetScheme::Double(position),
                size,
                variant: None,
                overlap: false,
                pip: false,
            }));
        }
    }

    items
}

/// Масштаб координат grid-payload (вендорный формат — 10000 единиц).
pub const GRID_SCALE: u32 = 10_000;

/// Слот фиксированной grid-геометрии (координаты в [`GRID_SCALE`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct GridSlot {
    /// Позиция окна (0-based).
    pub position: u32,
    /// Левый край.
    pub x: u32,
    /// Верхний край.
    pub y: u32,
    /// Ширина.
    pub width: u32,
    /// Высота.
    pub height: u32,
    /// Порядок наложения (0 — самый нижний).
    pub z_order: u32,
}

/// Payload grid-геометрии (форма вендорного `/video_layout_grids`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GridPayload {
    /// Ширина канваса в единицах [`GRID_SCALE`].
    pub width: u32,
    /// Высота канваса в единицах [`GRID_SCALE`].
    pub height: u32,
    /// Центрировать неполный последний ряд.
    pub centered_row: bool,
    /// Окна геометрии.
    pub slots: Vec<GridSlot>,
}

/// Флаги встроенной геометрии (нельзя удалить/изменить/отключить).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct GridDefaults {
    /// Можно ли удалить.
    pub deletable: bool,
    /// Можно ли редактировать.
    pub editable: bool,
    /// Можно ли отключить.
    pub disableable: bool,
}

impl GridDefaults {
    /// Встроенная геометрия: read-only.
    pub const fn fixed() -> Self {
        Self { deletable: false, editable: false, disableable: false }
    }
}

/// Встроенная grid-геометрия редактора раскладок.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GridInfo {
    /// Идентификатор геометрии.
    pub id: String,
    /// Отображаемое имя.
    pub display_name: String,
    /// Геометрия.
    pub payload: GridPayload,
    /// Флаги.
    pub defaults: GridDefaults,
}

/// Округляет пиксельную координату в единицы [`GRID_SCALE`].
fn grid_coord(value: f64) -> u32 {
    value.round().clamp(0.0, f64::from(GRID_SCALE)) as u32
}

/// Переводит пиксельные rect'ы 10000×10000 в слоты grid-payload.
pub fn slots_to_grid(rects: &[SlotRect]) -> Vec<GridSlot> {
    rects
        .iter()
        .enumerate()
        .map(|(index, rect)| {
            let x = grid_coord(rect.x);
            let y = grid_coord(rect.y);
            let width = grid_coord(rect.width).min(GRID_SCALE - x);
            let height = grid_coord(rect.height).min(GRID_SCALE - y);
            GridSlot { position: u32::try_from(index).unwrap_or(u32::MAX), x, y, width, height, z_order: 0 }
        })
        .collect()
}

/// Вендорные custom-геометрии редактора (`cols` — ширина сетки, `count` — окон).
const CUSTOM_GRIDS: [(&str, &str, usize, usize, usize); 6] = [
    ("custom_8x8_double_34", "8×8 Double 34", 8, 8, 34),
    ("custom_10x8_double_42", "10×8 Double 42", 10, 8, 42),
    ("custom_8x6_selected_37", "8×6 Selected 37", 8, 6, 37),
    ("custom_10x8_selected_77", "10×8 Selected 77", 10, 8, 77),
    ("custom_4x4_selected_13", "4×4 Selected 13", 4, 4, 13),
    ("custom_8x8_selected_61", "8×8 Selected 61", 8, 8, 61),
];

/// Легаси `preset_default_N` из прежнего плоского каталога (для get_grid).
const LEGACY_DEFAULT_SIZES: [u8; 10] = [1, 2, 3, 4, 6, 8, 9, 12, 13, 16];

/// Фиксированная сетка `cols × rows` c `count` окнами, заполняющая канвас.
fn custom_grid(id: &str, display_name: &str, cols: usize, capacity_rows: usize, count: usize) -> GridInfo {
    let cols = cols.max(1);
    let count = count.max(1).min(cols * capacity_rows.max(1));
    let rows = count.div_ceil(cols);
    let cell_width = GRID_SCALE / u32::try_from(cols).unwrap_or(u32::MAX);
    let cell_height = GRID_SCALE / u32::try_from(rows).unwrap_or(u32::MAX);
    let centered_row = !count.is_multiple_of(cols);
    let mut slots = Vec::with_capacity(count);
    for index in 0..count {
        let row = index / cols;
        let col = index % cols;
        let in_row = cols.min(count - row * cols);
        let row_offset = if centered_row && in_row < cols {
            (u32::try_from(cols - in_row).unwrap_or(0) * cell_width) / 2
        } else {
            0
        };
        let x = row_offset + u32::try_from(col).unwrap_or(0) * cell_width;
        let y = u32::try_from(row).unwrap_or(0) * cell_height;
        let width = if col + 1 == cols { GRID_SCALE - x } else { (GRID_SCALE - x).min(cell_width) };
        let height = if row + 1 == rows { GRID_SCALE - y } else { (GRID_SCALE - y).min(cell_height) };
        slots.push(GridSlot { position: u32::try_from(index).unwrap_or(u32::MAX), x, y, width, height, z_order: 0 });
    }
    GridInfo {
        id: id.to_string(),
        display_name: display_name.to_string(),
        payload: GridPayload { width: GRID_SCALE, height: GRID_SCALE, centered_row, slots },
        defaults: GridDefaults::fixed(),
    }
}

/// Встроенные grid-геометрии: 6 custom-схем редактора + легаси
/// `preset_default_N` (чтобы `get_grid` не ломался на старых id).
pub fn builtin_grids() -> Vec<GridInfo> {
    let mut grids: Vec<GridInfo> =
        CUSTOM_GRIDS.iter().map(|(id, name, cols, rows, count)| custom_grid(id, name, *cols, *rows, *count)).collect();
    for size in LEGACY_DEFAULT_SIZES {
        let preset = PresetId::equal(size);
        let slots = slots_to_grid(&geometry(&preset, f64::from(GRID_SCALE), f64::from(GRID_SCALE), 1.0, false));
        grids.push(GridInfo {
            id: format!("preset_default_{size}"),
            display_name: size.to_string(),
            payload: GridPayload { width: GRID_SCALE, height: GRID_SCALE, centered_row: false, slots },
            defaults: GridDefaults::fixed(),
        });
    }
    grids
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    fn parse(id: &str) -> PresetId {
        PresetId::parse(id).unwrap_or_else(|| panic!("{id} должен разбираться"))
    }

    fn inside(rect: &SlotRect, width: f64, height: f64) {
        assert!(rect.is_valid(), "{rect:?}: NaN/отрицательные координаты");
        assert!(rect.x + rect.width <= width + 1e-9, "{rect:?} выходит за ширину {width}");
        assert!(rect.y + rect.height <= height + 1e-9, "{rect:?} выходит за высоту {height}");
    }

    #[test]
    fn resolves_base_scheme_with_size_like_pnm_ui() {
        assert_eq!(resolve("preset_equal", Some(4)), "preset_equal_4");
        assert_eq!(resolve("preset_selected_t", Some(25)), "preset_selected_t_25");
        assert_eq!(resolve("preset_selected_b_4", Some(25)), "preset_selected_b_4");
        assert_eq!(resolve("preset_selected_b_4_(2)", Some(25)), "preset_selected_b_4_(2)");
        assert_eq!(resolve("preset_selected_rb_overlap", Some(2)), "preset_selected_rb_overlap_2");
        assert_eq!(resolve("preset_equal", None), "preset_equal");
        assert_eq!(resolve("", Some(4)), "");
        assert!(has_explicit_size("preset_equal_4"));
        assert!(has_explicit_size("preset_selected_b_4_(2)"));
        assert!(!has_explicit_size("preset_equal"));
        assert!(!has_explicit_size("preset_selected_rb_overlap"));
    }

    #[test]
    fn parses_vendor_preset_ids() {
        let variant = parse("preset_selected_b_4_(2)");
        assert_eq!(variant.scheme, PresetScheme::Selected(Position::B));
        assert_eq!(variant.size, 4);
        assert_eq!(variant.variant, Some(2));
        assert_eq!(variant.to_string(), "preset_selected_b_4_(2)");

        let equal = parse("preset_equal_25");
        assert_eq!(equal.scheme, PresetScheme::Equal);
        assert_eq!(equal.size, 25);
        assert_eq!(equal.to_string(), "preset_equal_25");

        let overlap = parse("preset_selected_rb_overlap");
        assert_eq!(overlap.scheme, PresetScheme::Selected(Position::Rb));
        assert_eq!(overlap.size, PresetId::DEFAULT_SIZE);
        assert!(overlap.overlap, "_overlap включает наложение");
        assert!(overlap.pip);

        let sized_overlap = parse("preset_selected_rb_overlap_2");
        assert_eq!(sized_overlap.size, 2);
        assert_eq!(sized_overlap.to_string(), "preset_selected_rb_overlap_2");

        assert_eq!(parse("preset_default_13").scheme, PresetScheme::Default);
        assert_eq!(parse("preset_double_rlr_10").scheme, PresetScheme::Double(Position::Rlr));
        // Bare-схемы вендорного списка получают размер по умолчанию.
        assert_eq!(parse("preset_equal").size, PresetId::DEFAULT_SIZE);
        assert_eq!(parse("preset_selected_b").size, PresetId::DEFAULT_SIZE);
        for invalid in ["", "equal_4", "preset_", "preset_unknown_4", "preset_equal_0", "preset_selected_zz_4"] {
            assert!(PresetId::parse(invalid).is_none(), "{invalid}: ожидался None");
        }
    }

    #[test]
    fn catalog_contains_required_vendor_presets() {
        let ids: HashSet<String> = catalog().into_iter().map(|info| info.id).collect();
        for required in [
            "preset_default_13",
            "preset_equal_1",
            "preset_equal_4",
            "preset_equal_25",
            "preset_selected_b_2",
            "preset_selected_b_4",
            "preset_selected_b_4_(2)",
            "preset_selected_rb_4",
            "preset_selected_rlr_16",
            "preset_selected_btb_25",
            "preset_selected_rb_overlap_2",
            "preset_selected_b_overlap_4",
            "preset_double_b_4",
            "preset_double_t_6",
            "preset_double_rlr_12",
        ] {
            assert!(ids.contains(required), "каталог должен содержать {required}");
        }
        let unique: HashSet<&str> = ids.iter().map(String::as_str).collect();
        assert_eq!(unique.len(), ids.len(), "id каталога не должны дублироваться");
    }

    #[test]
    fn every_catalog_preset_has_valid_geometry() {
        for info in catalog() {
            let preset = PresetId::parse(&info.id).unwrap_or_else(|| panic!("{} не разбирается", info.id));
            assert_eq!(preset.to_string(), info.id, "id каталога должен быть каноническим");
            for (width, height) in [(1280.0, 720.0), (1920.0, 1080.0), (640.0, 480.0)] {
                let slots = geometry(&preset, width, height, 1.0, false);
                assert_eq!(slots.len(), usize::from(info.size), "{}: число слотов", info.id);
                for slot in &slots {
                    inside(slot, width, height);
                }
            }
        }
    }

    #[test]
    fn geometry_is_inside_canvas_even_for_bad_dimensions() {
        let preset = parse("preset_equal_9");
        for (width, height) in [(f64::NAN, f64::NAN), (0.0, -5.0), (f64::INFINITY, 720.0)] {
            let slots = geometry(&preset, width, height, f64::NAN, false);
            assert_eq!(slots.len(), 9);
            for slot in &slots {
                inside(slot, 1280.0, 720.0);
            }
        }
    }

    #[test]
    fn schemes_and_variants_have_distinct_geometry() {
        let equal = geometry(&parse("preset_equal_4"), 1280.0, 720.0, 1.0, false);
        let selected = geometry(&parse("preset_selected_b_4"), 1280.0, 720.0, 1.0, false);
        let variant = geometry(&parse("preset_selected_b_4_(2)"), 1280.0, 720.0, 1.0, false);
        assert_ne!(equal, selected, "equal_4 и selected_b_4 должны различаться");
        assert_ne!(selected, variant, "вариант _(2) обязан менять геометрию");
        assert_ne!(equal, variant);
        // Первый слот selected — крупная зона, остальные — полоса мелких.
        assert!(selected[0].width * selected[0].height > selected[1].width * selected[1].height);
        // Пропорция 75/25 у варианта 1 меняется на 66/34 у варианта _(2).
        assert!(selected[0].height != variant[0].height);

        let double = geometry(&parse("preset_double_b_4"), 1280.0, 720.0, 1.0, false);
        assert_eq!(double.len(), 4);
        assert!(double[0].width * double[0].height > double[2].width * double[2].height);
    }

    #[test]
    fn overlap_presets_overlay_small_windows() {
        let preset = parse("preset_selected_rb_overlap_4");
        let slots = geometry(&preset, 1280.0, 720.0, 1.0, false);
        assert_eq!(slots.len(), 4);
        let big = slots[0];
        assert_eq!((big.x, big.y, big.width, big.height), (0.0, 0.0, 1280.0, 720.0));
        // Маленькие окна поверх большой зоны и внутри канваса.
        for slot in &slots[1..] {
            inside(slot, 1280.0, 720.0);
            assert!(slot.width < big.width && slot.height < big.height);
        }
        // `pip = true` форсирует наложение у обычного пресета.
        let forced = geometry(&parse("preset_equal_4"), 1280.0, 720.0, 1.0, true);
        assert_eq!(forced.len(), 4);
        assert_eq!(forced[0].width, 1280.0);
        assert!(forced[1].width < 1280.0);
    }

    #[test]
    fn builtin_grids_have_expected_geometry() {
        let grids = builtin_grids();
        let ids: HashSet<&str> = grids.iter().map(|grid| grid.id.as_str()).collect();
        assert_eq!(ids.len(), grids.len(), "id grid-геометрий уникальны");
        for (id, _, _, _, count) in CUSTOM_GRIDS {
            let grid = grids.iter().find(|grid| grid.id == id).unwrap_or_else(|| panic!("{id} отсутствует"));
            assert_eq!(grid.payload.slots.len(), count, "{id}: число окон");
            assert_eq!(grid.payload.width, GRID_SCALE);
            assert_eq!(grid.payload.height, GRID_SCALE);
            for slot in &grid.payload.slots {
                assert!(slot.x + slot.width <= GRID_SCALE, "{id}: слот выходит за канвас");
                assert!(slot.y + slot.height <= GRID_SCALE, "{id}: слот выходит за канвас");
            }
        }
        assert!(ids.contains("preset_default_13"));
        assert!(ids.contains("preset_default_4"));
    }
}
