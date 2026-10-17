//! Модель раскладок: [`Layout`], [`LayoutKind`], [`Slot`].
//!
//! Раскладка — абстрактное, рендер-независимое описание: набор слотов
//! с пиксельными rect на канвасе и назначением участника. Один и тот же
//! [`Layout`] применяется и MCU (пиксельные rect), и клиентом (проценты).
//!
//! Crate самодостаточен по типам-идентификаторам ([`ParticipantId`],
//! [`SlotId`]) — это opaque UUID-подобные идентификаторы (без внешней
//! зависимости от `vks-core`, чтобы остаться dep-light).

use crate::rect::Rect;
use serde::{Deserialize, Serialize};

/// Идентикатор участника в раскладке.
///
/// Opaque `u128` — совместим по значению с UUIDv7 (`uuid::Uuid`
/// сериализуется как string, а здесь как number — конвертация на
/// границе с `vks-core::ParticipantId`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ParticipantId(u128);

impl ParticipantId {
    /// Создаёт идентификатор из `u128`.
    pub fn new(value: u128) -> Self {
        Self(value)
    }

    /// Возвращает внутреннее значение.
    pub fn as_u128(&self) -> u128 {
        self.0
    }
}

impl std::fmt::Display for ParticipantId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "p{:032x}", self.0)
    }
}

/// Идентикатор слота в раскладке.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SlotId(u32);

impl SlotId {
    /// Создаёт идентификатор слота из `u32`.
    pub fn new(value: u32) -> Self {
        Self(value)
    }

    /// Возвращает внутреннее значение.
    pub fn as_u32(&self) -> u32 {
        self.0
    }
}

impl std::fmt::Display for SlotId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "slot-{0}", self.0)
    }
}

/// Вид раскладки (LAYOUT_ENGINE.md §1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LayoutKind {
    /// Балансированная сетка N тайлов.
    Grid,
    /// Один крупный фокус-тайл + полоса мелких.
    Focus,
    /// Один крупный центральный тайл + thumbnails.
    Speaker,
    /// Пользовательская (редактор).
    Custom,
}

/// Слот раскладки: rect + привязка участника.
///
/// `participant_id: None` — слот свободен (авто-назначение).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Slot {
    /// Идентикатор слота (стабильный в рамках layout).
    pub id: SlotId,
    /// Привязанный участник (None = свободный).
    pub participant_id: Option<ParticipantId>,
    /// Позиция и размер на канвасе (px).
    pub rect: Rect,
}

impl Slot {
    /// Создаёт слот (id, participant, rect). `id: u32` → [`SlotId`].
    #[must_use]
    pub fn new(id: u32, participant_id: Option<ParticipantId>, rect: Rect) -> Self {
        Self { id: SlotId::new(id), participant_id, rect }
    }
}

/// Раскладка — модель, сериализуемая (JSON для БД, protobuf для signaling).
///
/// `version` инкрементируется на каждое изменение (backward compat —
/// новые поля опциональные).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Layout {
    /// Version (инкремент на изменение).
    pub version: u32,
    /// Вид раскладки.
    pub kind: LayoutKind,
    /// Слоты (порядок = z-порядок отрисовки: первый — снизу).
    pub slots: Vec<Slot>,
}

impl Layout {
    /// Создаёт пустую раскладки.
    pub fn new(kind: LayoutKind) -> Self {
        Self { version: 1, kind, slots: Vec::new() }
    }

    /// Инкрементирует version (вызывается при каждом изменении).
    pub fn bump_version(&mut self) {
        self.version = self.version.saturating_add(1);
    }

    /// Возвращает slot по id, если есть.
    pub fn slot(&self, id: SlotId) -> Option<&Slot> {
        self.slots.iter().find(|s| s.id == id)
    }

    /// Возвращает slot по id (mut).
    pub fn slot_mut(&mut self, id: SlotId) -> Option<&mut Slot> {
        self.slots.iter_mut().find(|s| s.id == id)
    }

    /// Суммарная площадь всех слотов (px²).
    pub fn total_area(&self) -> u64 {
        self.slots.iter().map(|s| s.rect.area()).sum()
    }

    /// True, если два слота пересекаются (положительная площадь).
    /// Для grid-раскладок инвариант: `false` для любой пары.
    pub fn any_overlap(&self) -> bool {
        let slots = &self.slots;
        for i in 0..slots.len() {
            for j in (i + 1)..slots.len() {
                if slots[i].rect.intersects(&slots[j].rect) {
                    return true;
                }
            }
        }
        false
    }
}

/// Ошибки работы с раскладкой (валидация, apply).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum LayoutError {
    /// Canvas имеет нулевой размер.
    #[error("canvas has zero size")]
    EmptyCanvas,
    /// Раскладка не покрывает canvas полностью (grid).
    #[error("layout does not fully cover the canvas (gap of {0} px²)")]
    CoverageGap(u64),
    /// Два слота пересекаются (grid).
    #[error("slots {0} and {1} overlap")]
    OverlappingSlots(SlotId, SlotId),
    /// Фокус-участник не найден среди переданных.
    #[error("focus participant {0} not found in participant list")]
    FocusNotFound(ParticipantId),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slot(id: u32, x: u32, y: u32, w: u32, h: u32) -> Slot {
        Slot { id: SlotId::new(id), participant_id: None, rect: Rect::new(x, y, w, h) }
    }

    #[test]
    fn layout_new_and_bump() {
        let mut l = Layout::new(LayoutKind::Grid);
        assert_eq!(l.version, 1);
        assert_eq!(l.kind, LayoutKind::Grid);
        assert!(l.slots.is_empty());
        l.bump_version();
        assert_eq!(l.version, 2);
    }

    #[test]
    fn layout_slot_lookup() {
        let mut l = Layout::new(LayoutKind::Grid);
        l.slots.push(slot(0, 0, 0, 10, 10));
        l.slots.push(slot(1, 10, 0, 10, 10));
        assert!(l.slot(SlotId::new(0)).is_some());
        assert!(l.slot(SlotId::new(1)).is_some());
        assert!(l.slot(SlotId::new(2)).is_none());
        let s = l.slot_mut(SlotId::new(1)).unwrap();
        s.rect.x = 99;
        assert_eq!(l.slot(SlotId::new(1)).unwrap().rect.x, 99);
    }

    #[test]
    fn layout_total_area_and_overlap() {
        let l =
            Layout { version: 1, kind: LayoutKind::Grid, slots: vec![slot(0, 0, 0, 10, 10), slot(1, 10, 0, 10, 10)] };
        assert_eq!(l.total_area(), 200);
        assert!(!l.any_overlap());

        let l2 =
            Layout { version: 1, kind: LayoutKind::Custom, slots: vec![slot(0, 0, 0, 10, 10), slot(1, 5, 0, 10, 10)] };
        assert!(l2.any_overlap());
    }

    #[test]
    fn layout_serde_roundtrip() {
        let l = Layout {
            version: 7,
            kind: LayoutKind::Focus,
            slots: vec![slot(0, 0, 0, 100, 80), slot(1, 0, 80, 100, 20)],
        };
        let json = serde_json::to_string(&l).unwrap();
        let back: Layout = serde_json::from_str(&json).unwrap();
        assert_eq!(l, back);
        assert_eq!(l.kind, LayoutKind::Focus);
    }

    #[test]
    fn layout_kind_serde_snake_case() {
        for (kind, expected) in [
            (LayoutKind::Grid, "\"grid\""),
            (LayoutKind::Focus, "\"focus\""),
            (LayoutKind::Speaker, "\"speaker\""),
            (LayoutKind::Custom, "\"custom\""),
        ] {
            assert_eq!(serde_json::to_string(&kind).unwrap(), expected);
        }
    }

    #[test]
    fn participant_id_display_and_order() {
        let a = ParticipantId::new(1);
        let b = ParticipantId::new(2);
        assert!(a < b);
        assert_eq!(a.to_string(), "p00000000000000000000000000000001");
    }

    #[test]
    fn slot_id_display() {
        assert_eq!(SlotId::new(3).to_string(), "slot-3");
    }
}
