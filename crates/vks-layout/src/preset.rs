//! Пресеты раскладок TrueConf: идентификаторы, схемы и геометрия слотов.
//!
//! Вендорный фронтенд PNM адресует раскладки строковыми id вида
//! `preset_selected_b_4_(2)` / `preset_equal_25` / `preset_selected_rb_overlap`.
//! Этот модуль — единственный источник правды о том, как такой id разобрать
//! и какую геометрию он означает:
//!
//! - [`PresetId::parse`] — разбор id (схема + размер + суффиксы);
//! - [`geometry`] — пиксельные [`SlotRect`] на канвасе;
//! - [`builtin_grids`] — 6 встроенных custom-схем для `GET /api/v3/video_layout_grids`;
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
    /// Крупная зона вверху по центру, мелкие — по бокам и снизу.
    Rbl,
    /// Три яруса крупности: две крупные сверху, средние под ними, мелкие внизу.
    Triple,
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
            Self::Rbl => "rbl",
            Self::Triple => "triple",
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
            "rbl" => Self::Rbl,
            "triple" => Self::Triple,
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
    /// Вендорная палитра «квадрат»: четыре крупных окна в центре, мелкие вокруг.
    ///
    /// Это **отдельная** схема, а не синоним `equal`: у вендора нет
    /// `preset_equal_24`, а `preset_square_24` — это 4 крупных (2×2 в сетке 6×6)
    /// и 20 мелких. Приравнивание `square` к `equal` теряло схему при
    /// канонизации id и ломало геометрию в MCU.
    Square,
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
            "equal" => PresetScheme::Equal,
            "square" => PresetScheme::Square,
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
            PresetScheme::Square => "square".to_string(),
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
        // `selected_rbl`: крупная зона вверху по центру, мелкие — по бокам и
        // снизу (топология из иконки вендора: крупный блок 2×2 в сетке 4×4).
        Position::Rbl => rbl_slots(smalls),
        // `selected_triple` у вендора не встречается ⇒ как `selected_c`.
        Position::Triple => center_slots(variant, smalls),
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

/// `selected_rbl`: крупная зона вверху по центру, мелкие — по бокам и снизу.
///
/// Топология взята из иконки вендора `preset_selected_rbl_13`: сетка 4×4,
/// крупный блок 2×2 в верхней центральной части, остальные 12 ячеек — мелкие
/// окна, идут построчно.
fn rbl_slots(smalls: usize) -> Vec<SlotRect> {
    const GRID: u16 = 4;
    let cell = 1.0 / f64::from(GRID);
    let mut slots = vec![unit(cell, 0.0, 2.0 * cell, 2.0 * cell)];
    'cells: for row in 0..GRID {
        for col in 0..GRID {
            // Крупная зона занимает колонки 1-2 верхних двух строк.
            if row < 2 && (1..3).contains(&col) {
                continue;
            }
            if slots.len() > smalls {
                break 'cells;
            }
            slots.push(unit(f64::from(col) * cell, f64::from(row) * cell, cell, cell));
        }
    }
    slots.truncate(smalls + 1);
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
        Position::T | Position::Triple => {
            // Две крупные зоны сверху, полоса мелких снизу. `double_triple`
            // отличается третьим ярусом (средние окна), но вендорский эталон
            // есть только для ручного размера 11 ⇒ он берётся из MANUAL_SCHEMES.
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
        PresetScheme::Equal | PresetScheme::Default | PresetScheme::Square => Position::Rb,
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
        // `rbl`/`triple`: мелкие окна по центру (у вендора таких overlay-схем
        // нет — только ручные размеры, они берутся из MANUAL_SCHEMES).
        Position::Rbl | Position::Triple => (1.0 - box_width) / 2.0,
    };
    let box_y = match position {
        Position::T | Position::Rt => 0.0,
        Position::B | Position::Rlr | Position::Btb => 1.0 - box_height,
        Position::R | Position::L | Position::C => (1.0 - box_height) / 2.0,
        Position::Rb => 1.0 - box_height,
        Position::Rbl | Position::Triple => 1.0 - box_height,
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
/// Для вендорных manual-пресетов берётся эталонная схема
/// ([`geometry_for_manual`]); для dynamic/auto/неизвестных id — общий
/// эвристический fallback (`overlap_factor`/`pip` включают наложение).
/// Все rect внутри канваса, без NaN; пересечения возможны только у overlap.
pub fn geometry(preset: &PresetId, width: f64, height: f64, overlap_factor: f64, pip: bool) -> Vec<SlotRect> {
    let width = dimension(width, 1280.0);
    let height = dimension(height, 720.0);
    if let Some(slots) = geometry_for_manual(&preset.to_string(), width, height) {
        return slots.into_iter().map(|slot| slot.rect).collect();
    }
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
            // Эталон палитры «квадрат» известен только для ручного размера 24
            // (`preset_square_24` — 4 крупных в центре + 20 мелких), он берётся
            // из MANUAL_SCHEMES. Динамического эталона у вендора нет ⇒ сетка.
            PresetScheme::Square => equal_slots(count, preset.variant),
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

/// Один слот эталонной геометрии: прямоугольник + порядок наложения.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct CalculatedSlot {
    /// Пиксельная геометрия окна.
    pub rect: SlotRect,
    /// Порядок наложения (0 — нижний слой, 1 — окно поверх).
    pub z_order: u8,
}

/// true, если id входит в эталонный список manual-пресетов (65 шт.).
pub fn is_manual_preset(id: &str) -> bool {
    MANUAL_SCHEMES.iter().any(|(preset, _)| *preset == id)
}

/// true, если id — известный пресет геометрии: 65 вендорных + 54 Codian.
pub fn is_known_preset(id: &str) -> bool {
    is_manual_preset(id) || crate::codian::is_codian_preset(id)
}

fn safe_dimension(value: f64, fallback: f64) -> f64 {
    if value.is_finite() && value > 0.0 {
        value
    } else {
        fallback
    }
}

/// Пиксельный rect ячейки/блока сетки `cols × rows` по эталонной формуле
/// (`x = floor(coord * canvas / grid)` — так эталон распределяет остаток).
pub(crate) fn cell_rect(cell: (u16, u16, u16, u16), cols: u16, rows: u16, canvas: (f64, f64)) -> SlotRect {
    let (x, y, w, h) = cell;
    let (width, height) = canvas;
    let left = (width * f64::from(x) / f64::from(cols)).floor();
    let right = (width * f64::from(x + w) / f64::from(cols)).floor();
    let top = (height * f64::from(y) / f64::from(rows)).floor();
    let bottom = (height * f64::from(y + h) / f64::from(rows)).floor();
    SlotRect::new(left, top, right - left, bottom - top)
}

/// Эталонная геометрия manual-пресета (`None` для dynamic/auto/неизвестного id).
///
/// Явные сетки повторяют `preset_schemes._` эталона 2.2.1.66: блоки-«крупные
/// зоны» идут первыми, оставшиеся ячейки — построчно. Overlay-схемы хранят
/// доли канваса из live-ответа `calculate` (1280×720).
pub fn geometry_for_manual(id: &str, width: f64, height: f64) -> Option<Vec<CalculatedSlot>> {
    let (_, scheme) = MANUAL_SCHEMES.iter().find(|(preset, _)| *preset == id)?;
    let width = safe_dimension(width, 1280.0);
    let height = safe_dimension(height, 720.0);
    let mut slots = Vec::new();
    match scheme {
        ManualScheme::Grid { cols, rows, blocks } => {
            for &(x, y, w, h) in *blocks {
                slots.push(CalculatedSlot { rect: cell_rect((x, y, w, h), *cols, *rows, (width, height)), z_order: 0 });
            }
            for y in 0..*rows {
                for x in 0..*cols {
                    let covered =
                        blocks.iter().any(|&(bx, by, bw, bh)| bx <= x && x < bx + bw && by <= y && y < by + bh);
                    if !covered {
                        slots.push(CalculatedSlot {
                            rect: cell_rect((x, y, 1, 1), *cols, *rows, (width, height)),
                            z_order: 0,
                        });
                    }
                }
            }
        }
        ManualScheme::Explicit { cols, rows, slots: explicit } => {
            for &(x, y, w, h) in *explicit {
                slots.push(CalculatedSlot { rect: cell_rect((x, y, w, h), *cols, *rows, (width, height)), z_order: 0 });
            }
        }
        ManualScheme::Overlay { windows } => {
            for &(x, y, w, h) in *windows {
                let left = (x * width).round().clamp(0.0, width);
                let top = (y * height).round().clamp(0.0, height);
                let slot_width = (w * width).round().clamp(0.0, width - left);
                let slot_height = (h * height).round().clamp(0.0, height - top);
                let z_order = if w * h >= 0.5 { 0 } else { 1 };
                slots.push(CalculatedSlot { rect: SlotRect::new(left, top, slot_width, slot_height), z_order });
            }
        }
    }
    Some(slots)
}

/// Вендорные manual-пресеты (`preset_types._` + `preset_schemes._` эталона 2.2.1.66).
/// Порядок совпадает с эталонным списком `preset_types._`.
/// Все id вендорных manual-пресетов эталона 2.2.1.66 (65 шт.), в порядке эталона.
///
/// Единственный источник правды — таблица `MANUAL_SCHEMES`: отдельный список
/// id был её побайтовой копией и расходился с ней при каждой правке.
pub fn manual_preset_ids() -> impl Iterator<Item = &'static str> {
    MANUAL_SCHEMES.iter().map(|(preset, _)| *preset)
}

/// Эталонная схема manual-пресета: явная сетка (`Grid`/`Explicit`) или
/// оверлейные окна (`Overlay`, доли канваса 0..1).
enum ManualScheme {
    Grid { cols: u16, rows: u16, blocks: &'static [(u16, u16, u16, u16)] },
    Explicit { cols: u16, rows: u16, slots: &'static [(u16, u16, u16, u16)] },
    Overlay { windows: &'static [(f64, f64, f64, f64)] },
}

static MANUAL_SCHEMES: &[(&str, ManualScheme)] = &[
    ("preset_equal_1", ManualScheme::Grid { cols: 1, rows: 1, blocks: &[] }),
    ("preset_equal_2", ManualScheme::Explicit { cols: 2, rows: 4, slots: &[(0, 1, 1, 2), (1, 1, 1, 2)] }),
    ("preset_selected_b_3", ManualScheme::Grid { cols: 2, rows: 2, blocks: &[(0, 0, 2, 1)] }),
    ("preset_selected_t_3", ManualScheme::Grid { cols: 2, rows: 2, blocks: &[(0, 1, 2, 1)] }),
    ("preset_selected_r_3", ManualScheme::Grid { cols: 2, rows: 2, blocks: &[(0, 0, 1, 2)] }),
    ("preset_selected_l_3", ManualScheme::Grid { cols: 2, rows: 2, blocks: &[(1, 0, 1, 2)] }),
    ("preset_equal_3", ManualScheme::Explicit { cols: 4, rows: 2, slots: &[(0, 0, 2, 1), (2, 0, 2, 1), (1, 1, 2, 1)] }),
    ("preset_equal_4", ManualScheme::Grid { cols: 2, rows: 2, blocks: &[] }),
    ("preset_selected_r_4", ManualScheme::Grid { cols: 3, rows: 3, blocks: &[(0, 0, 2, 3)] }),
    ("preset_selected_l_4", ManualScheme::Grid { cols: 3, rows: 3, blocks: &[(1, 0, 2, 3)] }),
    ("preset_selected_b_4", ManualScheme::Grid { cols: 3, rows: 3, blocks: &[(0, 0, 3, 2)] }),
    ("preset_selected_t_4", ManualScheme::Grid { cols: 3, rows: 3, blocks: &[(0, 1, 3, 2)] }),
    (
        "preset_selected_b_4_(2)",
        ManualScheme::Explicit { cols: 3, rows: 9, slots: &[(0, 0, 3, 5), (0, 5, 1, 4), (1, 5, 1, 4), (2, 5, 1, 4)] },
    ),
    (
        "preset_selected_t_4_(2)",
        ManualScheme::Explicit { cols: 3, rows: 9, slots: &[(0, 0, 1, 4), (1, 0, 1, 4), (2, 0, 1, 4), (0, 4, 3, 5)] },
    ),
    ("preset_equal_6", ManualScheme::Grid { cols: 3, rows: 2, blocks: &[] }),
    ("preset_selected_rb_6", ManualScheme::Grid { cols: 3, rows: 3, blocks: &[(0, 0, 2, 2)] }),
    ("preset_selected_rt_6", ManualScheme::Grid { cols: 3, rows: 3, blocks: &[(0, 1, 2, 2)] }),
    ("preset_selected_r_5", ManualScheme::Grid { cols: 4, rows: 4, blocks: &[(0, 0, 3, 4)] }),
    ("preset_selected_l_5", ManualScheme::Grid { cols: 4, rows: 4, blocks: &[(1, 0, 3, 4)] }),
    ("preset_selected_b_5", ManualScheme::Grid { cols: 4, rows: 4, blocks: &[(0, 0, 4, 3)] }),
    ("preset_selected_t_5", ManualScheme::Grid { cols: 4, rows: 4, blocks: &[(0, 1, 4, 3)] }),
    ("preset_selected_rb_8", ManualScheme::Grid { cols: 4, rows: 4, blocks: &[(0, 0, 3, 3)] }),
    ("preset_selected_rt_8", ManualScheme::Grid { cols: 4, rows: 4, blocks: &[(0, 1, 3, 3)] }),
    ("preset_equal_8", ManualScheme::Grid { cols: 4, rows: 2, blocks: &[] }),
    ("preset_equal_9", ManualScheme::Grid { cols: 3, rows: 3, blocks: &[] }),
    ("preset_selected_b_9", ManualScheme::Grid { cols: 4, rows: 4, blocks: &[(0, 0, 4, 2)] }),
    ("preset_selected_btb_9", ManualScheme::Grid { cols: 4, rows: 4, blocks: &[(0, 1, 4, 2)] }),
    ("preset_selected_rlr_9", ManualScheme::Grid { cols: 4, rows: 4, blocks: &[(1, 0, 2, 4)] }),
    ("preset_selected_rb_9", ManualScheme::Grid { cols: 4, rows: 3, blocks: &[(0, 0, 2, 2)] }),
    ("preset_selected_rt_9", ManualScheme::Grid { cols: 4, rows: 3, blocks: &[(0, 1, 2, 2)] }),
    ("preset_equal_16", ManualScheme::Grid { cols: 4, rows: 4, blocks: &[] }),
    ("preset_selected_rb_13", ManualScheme::Grid { cols: 4, rows: 4, blocks: &[(0, 0, 2, 2)] }),
    ("preset_selected_rt_13", ManualScheme::Grid { cols: 4, rows: 4, blocks: &[(0, 2, 2, 2)] }),
    ("preset_double_btb_10", ManualScheme::Grid { cols: 4, rows: 4, blocks: &[(0, 1, 2, 2), (2, 1, 2, 2)] }),
    ("preset_double_b_10", ManualScheme::Grid { cols: 4, rows: 4, blocks: &[(0, 0, 2, 2), (2, 0, 2, 2)] }),
    ("preset_double_rlr_10", ManualScheme::Grid { cols: 4, rows: 4, blocks: &[(1, 0, 2, 2), (1, 2, 2, 2)] }),
    ("preset_double_r_10", ManualScheme::Grid { cols: 4, rows: 4, blocks: &[(0, 0, 2, 2), (0, 2, 2, 2)] }),
    ("preset_selected_btb_11", ManualScheme::Grid { cols: 5, rows: 5, blocks: &[(0, 1, 5, 3)] }),
    ("preset_selected_rlr_11", ManualScheme::Grid { cols: 5, rows: 5, blocks: &[(1, 0, 3, 5)] }),
    (
        "preset_double_triple_11",
        ManualScheme::Grid {
            cols: 6,
            rows: 6,
            blocks: &[(0, 0, 3, 3), (3, 0, 3, 3), (0, 3, 2, 2), (2, 3, 2, 2), (4, 3, 2, 2)],
        },
    ),
    ("preset_selected_c_13", ManualScheme::Grid { cols: 4, rows: 4, blocks: &[(1, 1, 2, 2)] }),
    ("preset_selected_rbl_13", ManualScheme::Grid { cols: 4, rows: 4, blocks: &[(1, 0, 2, 2)] }),
    ("preset_equal_20", ManualScheme::Grid { cols: 5, rows: 4, blocks: &[] }),
    (
        "preset_square_24",
        ManualScheme::Grid { cols: 6, rows: 6, blocks: &[(1, 1, 2, 2), (3, 1, 2, 2), (1, 3, 2, 2), (3, 3, 2, 2)] },
    ),
    ("preset_equal_25", ManualScheme::Grid { cols: 5, rows: 5, blocks: &[] }),
    ("preset_selected_rb_25", ManualScheme::Grid { cols: 7, rows: 7, blocks: &[(0, 0, 5, 5)] }),
    ("preset_selected_rt_25", ManualScheme::Grid { cols: 7, rows: 7, blocks: &[(0, 2, 5, 5)] }),
    ("preset_equal_36", ManualScheme::Grid { cols: 6, rows: 6, blocks: &[] }),
    ("preset_selected_rb_37", ManualScheme::Grid { cols: 8, rows: 6, blocks: &[(0, 0, 4, 3)] }),
    ("preset_selected_rb_57", ManualScheme::Grid { cols: 9, rows: 9, blocks: &[(0, 0, 5, 5)] }),
    ("preset_selected_rb_61", ManualScheme::Grid { cols: 10, rows: 8, blocks: &[(0, 0, 5, 4)] }),
    ("preset_selected_rb_77", ManualScheme::Grid { cols: 10, rows: 8, blocks: &[(0, 0, 2, 2)] }),
    ("preset_double_b_34", ManualScheme::Grid { cols: 8, rows: 8, blocks: &[(0, 0, 4, 4), (4, 0, 4, 4)] }),
    ("preset_double_b_42", ManualScheme::Grid { cols: 10, rows: 8, blocks: &[(0, 0, 5, 4), (5, 0, 5, 4)] }),
    ("preset_double_btb_34", ManualScheme::Grid { cols: 8, rows: 8, blocks: &[(0, 2, 4, 4), (4, 2, 4, 4)] }),
    ("preset_double_rlr_34", ManualScheme::Grid { cols: 8, rows: 8, blocks: &[(2, 0, 4, 4), (2, 4, 4, 4)] }),
    (
        "preset_selected_rb_overlap_2",
        ManualScheme::Overlay {
            windows: &[
                (0.000000000, 0.000000000, 1.000000000, 1.000000000),
                (0.687500000, 0.687500000, 0.250000000, 0.250000000),
            ],
        },
    ),
    (
        "preset_selected_rt_overlap_2",
        ManualScheme::Overlay {
            windows: &[
                (0.000000000, 0.000000000, 1.000000000, 1.000000000),
                (0.687500000, 0.062500000, 0.250000000, 0.250000000),
            ],
        },
    ),
    (
        "preset_selected_r_overlap_6",
        ManualScheme::Overlay {
            windows: &[
                (0.000000000, 0.000000000, 1.000000000, 1.000000000),
                (0.782812500, 0.050000000, 0.189062500, 0.180555556),
                (0.782812500, 0.230555556, 0.189062500, 0.180555556),
                (0.782812500, 0.411111111, 0.189062500, 0.177777778),
                (0.782812500, 0.588888889, 0.189062500, 0.180555556),
                (0.782812500, 0.769444444, 0.189062500, 0.180555556),
            ],
        },
    ),
    (
        "preset_selected_l_overlap_6",
        ManualScheme::Overlay {
            windows: &[
                (0.000000000, 0.000000000, 1.000000000, 1.000000000),
                (0.028125000, 0.050000000, 0.189062500, 0.180555556),
                (0.028125000, 0.230555556, 0.189062500, 0.180555556),
                (0.028125000, 0.411111111, 0.189062500, 0.177777778),
                (0.028125000, 0.588888889, 0.189062500, 0.180555556),
                (0.028125000, 0.769444444, 0.189062500, 0.180555556),
            ],
        },
    ),
    (
        "preset_selected_b_overlap_4",
        ManualScheme::Overlay {
            windows: &[
                (0.000000000, 0.000000000, 1.000000000, 1.000000000),
                (0.214062500, 0.794444444, 0.189062500, 0.180555556),
                (0.403125000, 0.794444444, 0.189062500, 0.180555556),
                (0.592187500, 0.794444444, 0.187500000, 0.180555556),
            ],
        },
    ),
    (
        "preset_selected_t_overlap_4",
        ManualScheme::Overlay {
            windows: &[
                (0.000000000, 0.000000000, 1.000000000, 1.000000000),
                (0.214062500, 0.050000000, 0.189062500, 0.180555556),
                (0.403125000, 0.050000000, 0.189062500, 0.180555556),
                (0.592187500, 0.050000000, 0.187500000, 0.180555556),
            ],
        },
    ),
    (
        "preset_double_b_overlap_5",
        ManualScheme::Overlay {
            windows: &[
                (0.000000000, 0.000000000, 0.500000000, 1.000000000),
                (0.500000000, 0.000000000, 0.500000000, 1.000000000),
                (0.214062500, 0.794444444, 0.189062500, 0.180555556),
                (0.403125000, 0.794444444, 0.189062500, 0.180555556),
                (0.592187500, 0.794444444, 0.187500000, 0.180555556),
            ],
        },
    ),
    (
        "preset_double_t_overlap_5",
        ManualScheme::Overlay {
            windows: &[
                (0.000000000, 0.000000000, 0.500000000, 1.000000000),
                (0.500000000, 0.000000000, 0.500000000, 1.000000000),
                (0.214062500, 0.050000000, 0.189062500, 0.180555556),
                (0.403125000, 0.050000000, 0.189062500, 0.180555556),
                (0.592187500, 0.050000000, 0.187500000, 0.180555556),
            ],
        },
    ),
    (
        "preset_selected_rlr_overlap_11",
        ManualScheme::Overlay {
            windows: &[
                (0.000000000, 0.000000000, 1.000000000, 1.000000000),
                (0.028125000, 0.050000000, 0.189062500, 0.180555556),
                (0.782812500, 0.050000000, 0.189062500, 0.180555556),
                (0.028125000, 0.230555556, 0.189062500, 0.180555556),
                (0.782812500, 0.230555556, 0.189062500, 0.180555556),
                (0.028125000, 0.411111111, 0.189062500, 0.177777778),
                (0.782812500, 0.411111111, 0.189062500, 0.177777778),
                (0.028125000, 0.588888889, 0.189062500, 0.180555556),
                (0.782812500, 0.588888889, 0.189062500, 0.180555556),
                (0.028125000, 0.769444444, 0.189062500, 0.180555556),
                (0.782812500, 0.769444444, 0.189062500, 0.180555556),
            ],
        },
    ),
];

/// Слот встроенной custom-геометрии (эталонный payload `video_layout_grids._`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct GridSlot {
    /// Левый край (в единицах сетки).
    pub x: u32,
    /// Верхний край.
    pub y: u32,
    /// Ширина.
    pub width: u32,
    /// Высота.
    pub height: u32,
}

/// Payload custom-геометрии (точная форма вендорного `/video_layout_grids`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GridPayload {
    /// Ширина сетки (единицы сетки, не пиксели).
    pub width: u32,
    /// Высота сетки.
    pub height: u32,
    /// Центрировать неполный последний ряд (`null` у вендорных схем).
    pub centered_row: Option<bool>,
    /// Окна геометрии.
    pub slots: Vec<GridSlot>,
}

/// Флаги встроенной геометрии.
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
    /// Вендорные `Custom scheme 1..6`: редактируемые/удаляемые, но не отключаемые.
    pub const fn builtin() -> Self {
        Self { deletable: true, editable: true, disableable: false }
    }
}

/// Встроенная grid-геометрия редактора раскладок.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GridInfo {
    /// Идентификатор геометрии (UUID эталона).
    pub id: String,
    /// Отображаемое имя.
    pub display_name: String,
    /// Геометрия.
    pub payload: GridPayload,
    /// Флаги.
    pub defaults: GridDefaults,
}

/// Слот эталонной custom-схемы в единицах сетки: `(x, y, width, height)`.
type GridSlotSpec = (u32, u32, u32, u32);
/// Строка эталонной custom-схемы: `(id, display_name, сетка, слоты)`.
type BuiltinGridSpec = (&'static str, &'static str, u32, u32, &'static [GridSlotSpec]);

/// Эталонные строки `video_layout_grids._` (live 2.2.1.66): id, имя, сетка, слоты.
const BUILTIN_GRIDS: [BuiltinGridSpec; 6] = [
    ("6fb877aa-92de-4b83-9f83-6c9386e06443", "Custom scheme 1", 8, 8, &[(0, 0, 4, 4), (4, 0, 4, 4)]),
    ("277103cc-57e1-4501-a2e6-7cd369b64e59", "Custom scheme 2", 10, 8, &[(0, 0, 5, 4), (5, 0, 5, 4)]),
    ("4ff117e7-cb6b-4396-9652-941b73535d20", "Custom scheme 3", 8, 6, &[(0, 0, 3, 4)]),
    ("eda71ad0-0dfe-46d1-8910-4c4b495afce1", "Custom scheme 4", 10, 8, &[(0, 0, 5, 4)]),
    ("45070592-a240-4e0a-9d5d-60dc365d7ce4", "Custom scheme 5", 10, 8, &[(0, 0, 2, 2)]),
    ("9bf78747-e1b2-42c9-b10e-1ece67a269e7", "Custom scheme 6", 4, 4, &[(1, 0, 2, 2)]),
];

/// Встроенные grid-геометрии: ровно 6 эталонных custom-схем.
pub fn builtin_grids() -> Vec<GridInfo> {
    BUILTIN_GRIDS
        .iter()
        .map(|(id, name, width, height, slots)| GridInfo {
            id: (*id).to_string(),
            display_name: (*name).to_string(),
            payload: GridPayload {
                width: *width,
                height: *height,
                centered_row: None,
                slots: slots.iter().map(|&(x, y, width, height)| GridSlot { x, y, width, height }).collect(),
            },
            defaults: GridDefaults::builtin(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use serde_json::{json, Value};

    use super::*;

    fn parse(id: &str) -> PresetId {
        PresetId::parse(id).unwrap_or_else(|| panic!("{id} должен разбираться"))
    }

    fn inside(rect: &SlotRect, width: f64, height: f64) {
        assert!(rect.is_valid(), "{rect:?}: NaN/отрицательные размеры");
        assert!(rect.x >= -1e-9 && rect.y >= -1e-9, "{rect:?}: отрицательная координата");
        assert!(rect.x + rect.width <= width + 1e-9, "{rect:?} выходит за ширину {width}");
        assert!(rect.y + rect.height <= height + 1e-9, "{rect:?} выходит за высоту {height}");
    }

    /// Все 92 вендорских id из бандла PNM 2.2.1.66
    /// (`chunks/index.bundle-*.js` — иконки `Tee`, `chunks/index6.bundle-*.js` —
    /// палитра редактора `mM` и подсказки `presetHints`): 65 ручных схем +
    /// 26 базовых имён + `preset_default_13`.
    const VENDOR_PRESET_IDS: [&str; 92] = [
        "preset_default_13",
        "preset_double_b",
        "preset_double_b_10",
        "preset_double_b_34",
        "preset_double_b_42",
        "preset_double_b_overlap",
        "preset_double_b_overlap_5",
        "preset_double_btb",
        "preset_double_btb_10",
        "preset_double_btb_34",
        "preset_double_r",
        "preset_double_r_10",
        "preset_double_rlr",
        "preset_double_rlr_10",
        "preset_double_rlr_34",
        "preset_double_t_overlap",
        "preset_double_t_overlap_5",
        "preset_double_triple",
        "preset_double_triple_11",
        "preset_equal",
        "preset_equal_1",
        "preset_equal_16",
        "preset_equal_2",
        "preset_equal_20",
        "preset_equal_25",
        "preset_equal_3",
        "preset_equal_36",
        "preset_equal_4",
        "preset_equal_6",
        "preset_equal_8",
        "preset_equal_9",
        "preset_selected_b",
        "preset_selected_b_3",
        "preset_selected_b_4",
        "preset_selected_b_4_(2)",
        "preset_selected_b_5",
        "preset_selected_b_9",
        "preset_selected_b_overlap",
        "preset_selected_b_overlap_4",
        "preset_selected_btb",
        "preset_selected_btb_11",
        "preset_selected_btb_9",
        "preset_selected_c",
        "preset_selected_c_13",
        "preset_selected_l",
        "preset_selected_l_3",
        "preset_selected_l_4",
        "preset_selected_l_5",
        "preset_selected_l_overlap",
        "preset_selected_l_overlap_6",
        "preset_selected_r",
        "preset_selected_r_3",
        "preset_selected_r_4",
        "preset_selected_r_5",
        "preset_selected_r_overlap",
        "preset_selected_r_overlap_6",
        "preset_selected_rb",
        "preset_selected_rb_13",
        "preset_selected_rb_25",
        "preset_selected_rb_37",
        "preset_selected_rb_57",
        "preset_selected_rb_6",
        "preset_selected_rb_61",
        "preset_selected_rb_77",
        "preset_selected_rb_8",
        "preset_selected_rb_9",
        "preset_selected_rb_overlap",
        "preset_selected_rb_overlap_2",
        "preset_selected_rbl",
        "preset_selected_rbl_13",
        "preset_selected_rlr",
        "preset_selected_rlr_11",
        "preset_selected_rlr_9",
        "preset_selected_rlr_overlap",
        "preset_selected_rlr_overlap_11",
        "preset_selected_rt",
        "preset_selected_rt_13",
        "preset_selected_rt_25",
        "preset_selected_rt_6",
        "preset_selected_rt_8",
        "preset_selected_rt_9",
        "preset_selected_rt_overlap",
        "preset_selected_rt_overlap_2",
        "preset_selected_t",
        "preset_selected_t_3",
        "preset_selected_t_4",
        "preset_selected_t_4_(2)",
        "preset_selected_t_5",
        "preset_selected_t_overlap",
        "preset_selected_t_overlap_4",
        "preset_square",
        "preset_square_24",
    ];

    fn slots_json(slots: &[CalculatedSlot]) -> Vec<Value> {
        slots
            .iter()
            .enumerate()
            .map(|(index, slot)| {
                json!({
                    "position": index + 1,
                    "x": slot.rect.x as u64,
                    "y": slot.rect.y as u64,
                    "width": slot.rect.width as u64,
                    "height": slot.rect.height as u64,
                    "z_order": slot.z_order,
                })
            })
            .collect()
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
        assert!(overlap.overlap);
        assert!(overlap.pip);

        let sized_overlap = parse("preset_selected_rb_overlap_2");
        assert_eq!(sized_overlap.size, 2);
        assert_eq!(sized_overlap.to_string(), "preset_selected_rb_overlap_2");

        assert_eq!(parse("preset_default_13").scheme, PresetScheme::Default);
        assert_eq!(parse("preset_double_rlr_10").scheme, PresetScheme::Double(Position::Rlr));
        assert_eq!(parse("preset_equal").size, PresetId::DEFAULT_SIZE);
        assert_eq!(parse("preset_selected_b").size, PresetId::DEFAULT_SIZE);
        for invalid in ["", "equal_4", "preset_", "preset_unknown_4", "preset_equal_0", "preset_selected_zz_4"] {
            assert!(PresetId::parse(invalid).is_none(), "{invalid}: ожидался None");
        }
    }

    /// Ровно 65 manual-пресетов эталона 2.2.1.66; dynamic/auto/базовые id не
    /// принимаются `calculate` и не должны попадать в список.
    #[test]
    fn manual_presets_match_reference() {
        let ids: Vec<&str> = manual_preset_ids().collect();
        assert_eq!(ids.len(), 65);
        let unique: HashSet<&str> = ids.iter().copied().collect();
        assert_eq!(unique.len(), ids.len(), "дубли id в списке manual-пресетов");
        for id in ids {
            assert!(is_manual_preset(id), "{id} отсутствует в MANUAL_SCHEMES");
            assert!(geometry_for_manual(id, 1280.0, 720.0).is_some(), "{id}: нет эталонной геометрии");
        }
        for dynamic in [
            "preset_default_13",
            "preset_equal",
            "preset_selected_b",
            "preset_double_btb",
            "preset_selected_rb_overlap",
            "preset_unknown_4",
        ] {
            assert!(!is_manual_preset(dynamic), "{dynamic} не является manual-пресетом");
            assert!(geometry_for_manual(dynamic, 1280.0, 720.0).is_none());
        }
    }

    /// Послотовая сверка с live-ответами `POST /api/v3/ui/video_layouts/calculate`
    /// эталона 192.168.1.92 (1280×720): 65 manual-пресетов, позиции 1..N.
    #[test]
    fn geometry_matches_reference_1280x720() {
        let fixture: Value = serde_json::from_str(include_str!("../tests/fixtures/reference_calculate_1280x720.json"))
            .expect("fixture эталонной геометрии");
        let expected = fixture.as_object().expect("объект id → slots");
        assert_eq!(expected.len(), manual_preset_ids().count());
        for id in manual_preset_ids() {
            let want = expected.get(id).unwrap_or_else(|| panic!("{id} отсутствует в fixture")).clone();
            let got = slots_json(&geometry_for_manual(id, 1280.0, 720.0).unwrap());
            assert_eq!(Value::Array(got), want, "{id}: расхождение с эталоном");
        }
    }

    /// Канонизация id не должна терять вендорскую схему.
    ///
    /// Раньше `preset_square_24` превращался в `preset_equal_24` (схемы
    /// `square` не было), `preset_double_triple_11` и `preset_selected_rbl_13`
    /// вообще не разбирались — три ручных пресета доходили до MCU с чужой
    /// геометрией. Теперь канонический id совпадает с вендорским, и таблица
    /// `MANUAL_SCHEMES` находится по тому же ключу, что и раньше.
    #[test]
    fn manual_preset_ids_survive_canonicalization() {
        for id in manual_preset_ids() {
            let parsed = PresetId::parse(id).unwrap_or_else(|| panic!("{id}: id не разбирается"));
            assert_eq!(parsed.to_string(), id, "{id}: канонизация меняет id");
        }
    }

    /// Все 92 вендорских id разбираются: словарь `Position`/`PresetScheme`
    /// покрывает словарь бандла PNM, иначе пресет молча уезжает в равную сетку.
    #[test]
    fn parses_all_vendor_preset_ids() {
        for id in VENDOR_PRESET_IDS {
            assert!(PresetId::parse(id).is_some(), "{id}: вендорский id не разбирается");
        }
    }

    /// Геометрия по канонизированному id совпадает с эталонной: цепочка
    /// «id → `PresetId` → id → `geometry_for_manual`» не теряет раскладку.
    #[test]
    fn canonical_geometry_matches_manual_reference() {
        for id in manual_preset_ids() {
            let parsed = PresetId::parse(id).unwrap();
            let canonical = parsed.to_string();
            let direct = geometry_for_manual(id, 1280.0, 720.0).expect("эталонная геометрия");
            let via_canonical = geometry_for_manual(&canonical, 1280.0, 720.0).expect("геометрия по канону");
            assert_eq!(slots_json(&direct), slots_json(&via_canonical), "{id}: канон меняет геометрию");
        }
    }

    /// Property-тест геометрии: число окон, положительная площадь, канвас,
    /// 1-based позиции и отсутствие пересечений у неперекрывающихся схем.
    #[test]
    fn geometry_properties_hold_for_all_manual_presets() {
        let fixture: Value = serde_json::from_str(include_str!("../tests/fixtures/reference_calculate_1280x720.json"))
            .expect("fixture эталонной геометрии");
        let reference = fixture.as_object().expect("объект id → slots");
        for id in manual_preset_ids() {
            let expected = reference
                .get(id)
                .and_then(Value::as_array)
                .map(Vec::len)
                .unwrap_or_else(|| panic!("{id} отсутствует в fixture"));
            for (width, height) in [(1280.0, 720.0), (1920.0, 1080.0), (640.0, 480.0), (1000.0, 700.0)] {
                let slots = geometry_for_manual(id, width, height).unwrap();
                assert_eq!(slots.len(), expected, "{id}: число окон на {width}×{height}");
                let mut area = 0.0;
                for (index, slot) in slots.iter().enumerate() {
                    inside(&slot.rect, width, height);
                    assert!(slot.rect.width > 0.0 && slot.rect.height > 0.0, "{id}: пустой слот {}", index + 1);
                    area += slot.rect.width * slot.rect.height;
                }
                // У pip/overlap-схем окна лежат поверх крупной зоны — сумма
                // площадей законно больше канваса; проверяем только неперекрытие.
                if slots.iter().all(|slot| slot.z_order == 0) {
                    assert!(area <= width * height + 1e-6, "{id}: сумма площадей больше канваса");
                }
                // Пересекаться могут только окна поверх крупной зоны (z_order=1).
                for (i, left) in slots.iter().enumerate() {
                    for right in &slots[i + 1..] {
                        assert!(
                            left.z_order > 0 || right.z_order > 0 || !intersects(&left.rect, &right.rect),
                            "{id}: пересечение слотов {} и {}",
                            i + 1,
                            i + 2
                        );
                    }
                }
            }
        }
    }

    fn intersects(a: &SlotRect, b: &SlotRect) -> bool {
        let epsilon = 1e-6;
        a.x + a.width > b.x + epsilon
            && b.x + b.width > a.x + epsilon
            && a.y + a.height > b.y + epsilon
            && b.y + b.height > a.y + epsilon
    }

    /// Ключевые пресеты 2×2/3×3/25/34: точные пиксели эталона.
    #[test]
    fn reference_schemes_have_expected_pixels() {
        let slots = |id: &str| geometry_for_manual(id, 1280.0, 720.0).unwrap();
        let rects = |id: &str| slots(id).iter().map(|slot| slot.rect).collect::<Vec<_>>();

        assert_eq!(
            rects("preset_equal_2"),
            vec![SlotRect::new(0.0, 180.0, 640.0, 360.0), SlotRect::new(640.0, 180.0, 640.0, 360.0)]
        );
        assert_eq!(
            rects("preset_equal_3"),
            vec![
                SlotRect::new(0.0, 0.0, 640.0, 360.0),
                SlotRect::new(640.0, 0.0, 640.0, 360.0),
                SlotRect::new(320.0, 360.0, 640.0, 360.0),
            ]
        );
        let selected = rects("preset_selected_b_4");
        assert_eq!(selected[0], SlotRect::new(0.0, 0.0, 1280.0, 480.0), "крупная зона сверху 2/3");
        assert_eq!(selected[1], SlotRect::new(0.0, 480.0, 426.0, 240.0));
        assert_eq!(selected[2], SlotRect::new(426.0, 480.0, 427.0, 240.0));
        assert_eq!(selected[3], SlotRect::new(853.0, 480.0, 427.0, 240.0));

        let variant = rects("preset_selected_b_4_(2)");
        assert_eq!(variant[0], SlotRect::new(0.0, 0.0, 1280.0, 400.0), "вариант 5/9");
        assert_eq!(variant[1].height, 320.0);

        assert_eq!(rects("preset_selected_rb_25")[0], SlotRect::new(0.0, 0.0, 914.0, 514.0));
        assert_eq!(
            rects("preset_double_b_10")[..2],
            [SlotRect::new(0.0, 0.0, 640.0, 360.0), SlotRect::new(640.0, 0.0, 640.0, 360.0),]
        );
        assert_eq!(rects("preset_square_24")[0], SlotRect::new(213.0, 120.0, 427.0, 240.0));
        assert_eq!(rects("preset_selected_rb_37")[0], SlotRect::new(0.0, 0.0, 640.0, 360.0));
        assert_eq!(rects("preset_selected_rb_77")[0], SlotRect::new(0.0, 0.0, 256.0, 180.0));
    }

    #[test]
    fn overlap_schemes_are_full_canvas_with_pip_windows() {
        let slots = geometry_for_manual("preset_selected_b_overlap_4", 1280.0, 720.0).unwrap();
        assert_eq!(slots.len(), 4);
        assert_eq!(slots[0].rect, SlotRect::new(0.0, 0.0, 1280.0, 720.0));
        assert_eq!(slots[0].z_order, 0);
        assert_eq!(slots[1].rect, SlotRect::new(274.0, 572.0, 242.0, 130.0));
        assert_eq!(slots[1].z_order, 1);

        let pip = geometry_for_manual("preset_selected_rb_overlap_2", 1280.0, 720.0).unwrap();
        assert_eq!(pip.len(), 2);
        assert_eq!(pip[1].rect, SlotRect::new(880.0, 495.0, 320.0, 180.0));
        assert_eq!(pip[1].z_order, 1);

        // double-overlap: две крупные половины и окна поверх.
        let double = geometry_for_manual("preset_double_b_overlap_5", 1280.0, 720.0).unwrap();
        assert_eq!(double.len(), 5);
        assert_eq!(double[0].rect, SlotRect::new(0.0, 0.0, 640.0, 720.0));
        assert_eq!(double[1].rect, SlotRect::new(640.0, 0.0, 640.0, 720.0));
        assert!(double[2..].iter().all(|slot| slot.z_order == 1));
    }

    #[test]
    fn geometry_is_inside_canvas_even_for_bad_dimensions() {
        for id in ["preset_equal_9", "preset_selected_b_4_(2)", "preset_selected_rb_overlap_2"] {
            let slots = geometry_for_manual(id, f64::NAN, 0.0).unwrap();
            for slot in slots {
                inside(&slot.rect, 1280.0, 720.0);
            }
        }
    }

    #[test]
    fn builtin_grids_match_reference_contract() {
        let grids = builtin_grids();
        assert_eq!(grids.len(), 6);
        let expected = [
            ("6fb877aa-92de-4b83-9f83-6c9386e06443", "Custom scheme 1", 8, 8, 2),
            ("277103cc-57e1-4501-a2e6-7cd369b64e59", "Custom scheme 2", 10, 8, 2),
            ("4ff117e7-cb6b-4396-9652-941b73535d20", "Custom scheme 3", 8, 6, 1),
            ("eda71ad0-0dfe-46d1-8910-4c4b495afce1", "Custom scheme 4", 10, 8, 1),
            ("45070592-a240-4e0a-9d5d-60dc365d7ce4", "Custom scheme 5", 10, 8, 1),
            ("9bf78747-e1b2-42c9-b10e-1ece67a269e7", "Custom scheme 6", 4, 4, 1),
        ];
        for (grid, (id, name, width, height, slots)) in grids.iter().zip(expected) {
            assert_eq!(grid.id, id);
            assert_eq!(grid.display_name, name);
            assert_eq!(grid.payload.width, width);
            assert_eq!(grid.payload.height, height);
            assert_eq!(grid.payload.centered_row, None);
            assert_eq!(grid.payload.slots.len(), slots);
            assert_eq!(grid.defaults, GridDefaults { deletable: true, editable: true, disableable: false });
        }
        let unique: HashSet<&str> = grids.iter().map(|grid| grid.id.as_str()).collect();
        assert_eq!(unique.len(), grids.len());
    }
}
