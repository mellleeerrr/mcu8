//! Модель раскладок: [`Layout`], [`LayoutKind`], [`Slot`].
//!
//! Раскладка — абстрактное, рендер-независимое описание: набор слотов
//! с пиксельными rect на канвасе и назначением участника. Один и тот же
//! [`Layout`] применяется и MCU (пиксельные rect), и клиентом (проценты).
//!
//! Crate самодостаточен по типам-идентификаторам ([`ParticipantId`],
//! [`SlotId`]) — это opaque UUID-подобные идентификаторы (без внешней
//! зависимости от `vks-core`, чтобы остаться dep-light).

use crate::rect::{Canvas, NormalizedRect, Rect};
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

impl LayoutKind {
    /// Строковое имя вида раскладки — то же, что в протоколе
    /// (`signaling.proto`: `kind` в `Layout`) и в serde.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Grid => "grid",
            Self::Focus => "focus",
            Self::Speaker => "speaker",
            Self::Custom => "custom",
        }
    }

    /// Разбор строки вида раскладки (неизвестное имя ⇒ `Custom`).
    #[must_use]
    pub fn parse(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "grid" => Self::Grid,
            "focus" => Self::Focus,
            "speaker" => Self::Speaker,
            _ => Self::Custom,
        }
    }
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
    /// z-порядок отрисовки слота (M0.4).
    #[serde(default)]
    pub z_index: i32,
    /// Закреплен ли слот (M0.4).
    #[serde(default)]
    pub pinned: bool,
    /// Является ли слот фокусным (M0.4).
    #[serde(default)]
    pub is_focus: bool,
}

impl Slot {
    /// Создаёт слот (id, participant, rect). `id: u32` → [`SlotId`].
    #[must_use]
    pub fn new(id: u32, participant_id: Option<ParticipantId>, rect: Rect) -> Self {
        Self { id: SlotId::new(id), participant_id, rect, z_index: 0, pinned: false, is_focus: false }
    }

    /// Создаёт слот с дополнительными атрибутами (M0.4).
    #[must_use]
    pub fn with_attributes(
        id: u32,
        participant_id: Option<ParticipantId>,
        rect: Rect,
        z_index: i32,
        pinned: bool,
        is_focus: bool,
    ) -> Self {
        Self { id: SlotId::new(id), participant_id, rect, z_index, pinned, is_focus }
    }
}

/// Нормализованный слот раскладки (M0.4).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct NormalizedSlot {
    /// Идентикатор слота.
    pub id: SlotId,
    /// Привязанный участник.
    pub participant_id: Option<ParticipantId>,
    /// Позиция и размер в нормализованных координатах `[0.0, 1.0]`.
    pub rect: NormalizedRect,
    /// z-порядок отрисовки слота.
    #[serde(default)]
    pub z_index: i32,
    /// Закреплен ли слот.
    #[serde(default)]
    pub pinned: bool,
    /// Является ли слот фокусным.
    #[serde(default)]
    pub is_focus: bool,
}

/// Каноническая нормализованная раскладка (M0.4).
///
/// Сериализуемый контракт раскладки, не зависящий от физического разрешения
/// канваса. MCU и SDK масштабируют её на нужный канвас через [`NormalizedLayout::scale_to_canvas`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NormalizedLayout {
    /// Version (инкремент на изменение).
    pub version: u32,
    /// Вид раскладки.
    pub kind: LayoutKind,
    /// Нормализованные слоты.
    pub slots: Vec<NormalizedSlot>,
}

impl NormalizedLayout {
    /// Масштабирует нормализованную раскладку в пиксельную [`Layout`] на заданном [`Canvas`].
    pub fn scale_to_canvas(&self, canvas: &Canvas) -> Layout {
        let slots = self
            .slots
            .iter()
            .map(|s| Slot {
                id: s.id,
                participant_id: s.participant_id,
                rect: s.rect.to_rect(canvas),
                z_index: s.z_index,
                pinned: s.pinned,
                is_focus: s.is_focus,
            })
            .collect();
        Layout { version: self.version, kind: self.kind, slots }
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

    /// Преобразует пиксельную раскладку в нормализованную [`NormalizedLayout`] на заданном [`Canvas`] (M0.4).
    pub fn to_normalized(&self, canvas: &Canvas) -> NormalizedLayout {
        let slots = self
            .slots
            .iter()
            .map(|s| NormalizedSlot {
                id: s.id,
                participant_id: s.participant_id,
                rect: s.rect.to_normalized(canvas),
                z_index: s.z_index,
                pinned: s.pinned,
                is_focus: s.is_focus,
            })
            .collect();
        NormalizedLayout { version: self.version, kind: self.kind, slots }
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
        Slot {
            id: SlotId::new(id),
            participant_id: None,
            rect: Rect::new(x, y, w, h),
            z_index: 0,
            pinned: false,
            is_focus: false,
        }
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

    #[test]
    fn custom_slot_ids_and_attributes() {
        let mut layout = Layout::new(LayoutKind::Custom);
        layout.slots.push(Slot::with_attributes(
            7,
            Some(ParticipantId::new(1007)),
            Rect::new(0, 0, 100, 100),
            1,
            true,
            false,
        ));
        layout.slots.push(Slot::with_attributes(
            42,
            Some(ParticipantId::new(1042)),
            Rect::new(100, 0, 200, 100),
            2,
            false,
            true,
        ));

        assert_eq!(layout.slot(SlotId::new(7)).unwrap().z_index, 1);
        assert!(layout.slot(SlotId::new(7)).unwrap().pinned);
        assert!(!layout.slot(SlotId::new(7)).unwrap().is_focus);

        assert_eq!(layout.slot(SlotId::new(42)).unwrap().z_index, 2);
        assert!(!layout.slot(SlotId::new(42)).unwrap().pinned);
        assert!(layout.slot(SlotId::new(42)).unwrap().is_focus);
    }

    #[test]
    fn backward_compatibility_old_slot_json_deserialization() {
        let legacy_json = r#"{
            "version": 1,
            "kind": "grid",
            "slots": [
                {
                    "id": 7,
                    "participant_id": null,
                    "rect": { "x": 0, "y": 0, "w": 640, "h": 480 }
                },
                {
                    "id": 42,
                    "participant_id": 12345,
                    "rect": { "x": 640, "y": 0, "w": 640, "h": 480 }
                }
            ]
        }"#;

        let parsed: Layout = serde_json::from_str(legacy_json).expect("should deserialize legacy json without error");
        assert_eq!(parsed.slots.len(), 2);
        let s7 = parsed.slot(SlotId::new(7)).unwrap();
        assert_eq!(s7.z_index, 0);
        assert!(!s7.pinned);
        assert!(!s7.is_focus);

        let s42 = parsed.slot(SlotId::new(42)).unwrap();
        assert_eq!(s42.participant_id, Some(ParticipantId::new(12345)));
        assert_eq!(s42.z_index, 0);
        assert!(!s42.pinned);
        assert!(!s42.is_focus);
    }

    #[test]
    fn normalized_layout_scale_across_canvases() {
        let canvas1080 = Canvas::new(1920, 1080);
        let canvas720 = Canvas::new(1280, 720);
        let canvas360 = Canvas::new(640, 360);

        let mut l = Layout::new(LayoutKind::Focus);
        l.slots.push(Slot::with_attributes(7, Some(ParticipantId::new(7)), Rect::new(0, 0, 1920, 810), 1, true, true));
        l.slots.push(Slot::with_attributes(
            42,
            Some(ParticipantId::new(42)),
            Rect::new(0, 810, 1920, 270),
            0,
            false,
            false,
        ));

        let norm = l.to_normalized(&canvas1080);
        assert_eq!(norm.slots.len(), 2);
        assert_eq!(norm.slots[0].id, SlotId::new(7));
        assert_eq!(norm.slots[1].id, SlotId::new(42));
        assert_eq!(norm.slots[0].z_index, 1);
        assert!(norm.slots[0].pinned);
        assert!(norm.slots[0].is_focus);

        let scaled720 = norm.scale_to_canvas(&canvas720);
        let s7_720 = scaled720.slot(SlotId::new(7)).unwrap();
        assert_eq!(s7_720.rect, Rect::new(0, 0, 1280, 540));
        assert_eq!(s7_720.participant_id, Some(ParticipantId::new(7)));
        assert_eq!(s7_720.z_index, 1);
        assert!(s7_720.pinned);
        assert!(s7_720.is_focus);

        let scaled360 = norm.scale_to_canvas(&canvas360);
        let s42_360 = scaled360.slot(SlotId::new(42)).unwrap();
        assert_eq!(s42_360.rect, Rect::new(0, 270, 640, 90));
        assert_eq!(s42_360.participant_id, Some(ParticipantId::new(42)));
        assert_eq!(s42_360.z_index, 0);
        assert!(!s42_360.pinned);
        assert!(!s42_360.is_focus);
    }
}
