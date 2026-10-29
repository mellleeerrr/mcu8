//! `vks-layout` — движок раскладок (используется и MCU-сервером, и клиентом).
//!
//! Фаза 4 (DEVELOPMENT_PLAN.md): самодостаточное ядро — модель раскладки
//! (slots, participants, focus) + математика расстановки (grid/focus/speaker).
//!
//! Нет canvas/GL — чистая геометрия, полностью тестируемая.
//!
//! Источник: `docs/architecture/LAYOUT_ENGINE.md` (ADR-0012).
//!
//! ## Модули
//! - `core` — [`Layout`], [`LayoutKind`], [`Slot`], `ParticipantId`, `SlotId`.
//! - `rect` — [`Rect`], [`Canvas`] (пиксельная геометрия).
//! - `grid` — [`GridLayout`]: N тайлов в балансированную сетку.
//! - `focus` — [`FocusLayout`]: крупный фокус + полоса мелких.
//! - [`preset`] — вендорные пресеты PNM: id, схемы и геометрия слотов.
//! - [`codian`] — 52 раскладки Codian MCU 4200 + 2 кастомные.
//! - [`grids`] — конструктор «Схема расположения участников»: `vks_grid_*`, `vks_pin_*`.
//! - [`roles`] — роли окон (default/fixed/active/rolling) и назначение участников.
//!
//! ## Инварианты
//! - Все rect в пикселях на canvas (общая валюта MCU/клиента).
//! - Grid: rects не пересекаются; полные ряды покрывают ширину, неполный
//!   последний ряд центрируется (для n, кратного cols, — canvas целиком);
//!   на холсте уже `cols` ряд делит свою ширину; положительна последняя плитка
//!   ряда, остальные могут быть нулевой ширины (холст уже `cols`).
//! - Focus/Speaker: фокус > 50% площади, остальные — мелкие.
//! - `Layout.version` — инкремент на каждое изменение (backward compat).
//! - **No `unsafe`** (CODE_STYLE §6).
//!
//! Дерево: vks-core (концептуально); по типам-идентификаторам крейт
//! самодостаточен (dep-light: только serde + thiserror).

pub mod codian;
mod core;
mod focus;
mod grid;
pub mod grids;
pub mod preset;
mod rect;
pub mod roles;

pub use codian::{CodianScheme, Pip, Region};
pub use core::{Layout, LayoutError, LayoutKind, NormalizedLayout, NormalizedSlot, ParticipantId, Slot, SlotId};
pub use focus::{main_window, FocusLayout};
pub use grid::GridLayout;
pub use preset::{
    CalculatedSlot, GridDefaults, GridInfo, GridPayload, GridSlot, Position, PresetId, PresetScheme, SlotRect,
};
pub use rect::{Canvas, NormalizedRect, Rect};
pub use roles::{assign_by_role, RotationState, SlotRole};

/// Размер, **явно** закодированный в id пресета: параметрический конструктор
/// (`vks_grid_25`), Codian-схема (`codian_07`) или вендорский `preset_*`
/// с числовым хвостом (`preset_default_13`, `preset_equal_25`, `preset_equal_25_(2)`).
///
/// `None` для id без размера (`preset_equal` — вендорский дефолт): вызывающий
/// сам решает, брать ли `payload.size`. Это важно для wire-контракта движка:
/// `preset_equal` + `size:4` обязан остаться 4 окнами, а не дефолтными 13.
pub fn preset_size(id: &str) -> Option<usize> {
    if let Some(count) = grids::slot_count(id) {
        return Some(count);
    }
    if let Some(count) = codian::slot_count(id) {
        return Some(count);
    }
    let base = id.rsplit_once("_(").map_or(id, |(base, _)| base);
    let tail = base.strip_prefix("preset_")?.rsplit('_').next()?;
    // Untrusted ids come from PNM payloads: keep the parsed capacity inside the
    // supported window range instead of trusting arbitrary integers.
    tail.parse::<usize>().ok().filter(|count| (1..=grids::MAX_WINDOWS).contains(count))
}

#[cfg(test)]
mod preset_size_tests {
    #[test]
    fn resolves_constructor_codian_and_vendor_ids() {
        assert_eq!(super::preset_size("vks_grid_25"), Some(25));
        assert_eq!(super::preset_size("vks_pin_l_g10_m20_8"), Some(8));
        assert_eq!(super::preset_size("codian_07"), Some(super::codian::slot_count("codian_07").unwrap()));
        assert_eq!(super::preset_size("preset_default_13"), Some(13));
        assert_eq!(super::preset_size("preset_equal_25"), Some(25));
        assert_eq!(super::preset_size("preset_equal_25_(2)"), Some(25));
        assert_eq!(super::preset_size("preset_equal"), None, "id без размера — не выдумываем дефолт");
        assert_eq!(super::preset_size("unknown_preset"), None);
    }
}
