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
//! - [`core`] — [`Layout`], [`LayoutKind`], [`Slot`], `ParticipantId`, `SlotId`.
//! - [`rect`] — [`Rect`], [`Canvas`] (пиксельная геометрия).
//! - [`grid`] — [`GridLayout`]: N тайлов в балансированную сетку.
//! - [`focus`] — [`FocusLayout`]: крупный фокус + полоса мелких.
//! - [`preset`] — вендорные пресеты PNM: id, схемы и геометрия слотов.
//! - [`codian`] — 52 раскладки Codian MCU 4200 + 2 кастомные.
//! - [`roles`] — роли окон (default/fixed/active/rolling) и назначение участников.
//! - [`viewer`] — план раскладки для зрителя (ADR-0029, §2 Codian).
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

pub mod codian;
mod core;
mod focus;
mod grid;
pub mod preset;
mod rect;
pub mod roles;
pub mod viewer;

pub use codian::{CodianScheme, Pip, Region};
pub use core::{Layout, LayoutError, LayoutKind, NormalizedLayout, NormalizedSlot, ParticipantId, Slot, SlotId};
pub use focus::{main_window, promote_to_main_window, FocusLayout};
pub use grid::GridLayout;
pub use preset::{
    CalculatedSlot, GridDefaults, GridInfo, GridPayload, GridSlot, Position, PresetId, PresetScheme, SlotRect,
};
pub use rect::{Canvas, NormalizedRect, Rect};
pub use roles::{assign_by_role, RotationState, SlotRole};
pub use viewer::{plan_viewer, PlanCell, SelfViewPolicy, ViewerContext, ViewerPlan};
