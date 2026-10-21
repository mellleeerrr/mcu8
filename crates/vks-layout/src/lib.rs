//! `vks-layout` — движок раскладок (используется и MCU-сервером, и клиентом).
//!
//! Фаза 4 (DEVELOPMENT_PLAN.md): самодостаточное ядро — модель раскладки
//! (slots, participants, focus) + математика расстановки (grid/focus/speaker)
//! + рендерер (план композиции `DrawCommand`).
//!
//! Нет canvas/GL — чистая геометрия, полностью тестируемая.
//!
//! Источник: `docs/architecture/LAYOUT_ENGINE.md` (ADR-0012).
//!
//! ## Модули
//! - [`core`] — [`Layout`], [`LayoutKind`], [`Slot`], `ParticipantId`, `SlotId`.
//! - [`rect`] — [`Rect`], [`Canvas`] (пиксельная геометрия).
//! - [`grid`] — [`GridLayout`]: N тайлов в балансированную сетку.
//! - [`focus`] — [`FocusLayout`]: крупный фокус + полоса мелких.
//! - [`renderer`] — [`Renderer`]: Layout + frame-meta → `Vec<DrawCommand>`.
//! - [`preset`] — вендорные пресеты PNM: id, схемы и геометрия слотов.
//!
//! ## Инварианты
//! - Все rect в пикселях на canvas (общая валюта MCU/клиента).
//! - Grid: rects не пересекаются, покрывают canvas целиком.
//! - Focus/Speaker: фокус > 50% площади, остальные — мелкие.
//! - `Layout.version` — инкремент на каждое изменение (backward compat).
//! - **No `unsafe`** (CODE_STYLE §6).
//!
//! Дерево: vks-core (концептуально); по типам-идентификаторам крейт
//! самодостаточен (dep-light: только serde + thiserror).

mod core;
mod focus;
mod grid;
pub mod preset;
mod rect;
mod renderer;

pub use core::{Layout, LayoutError, LayoutKind, NormalizedLayout, NormalizedSlot, ParticipantId, Slot, SlotId};
pub use focus::FocusLayout;
pub use grid::GridLayout;
pub use preset::{
    GridDefaults, GridInfo, GridPayload, GridSlot, Position, PresetId, PresetInfo, PresetScheme, SlotRect,
};
pub use rect::{Canvas, NormalizedRect, Rect};
pub use renderer::{DrawCommand, FrameMeta, Renderer};
