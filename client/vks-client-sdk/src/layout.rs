//! Приём раскладки комнаты от сервера (`LayoutChanged`).
//!
//! Сервер рассылает раскладку в нормализованных координатах `[0.0, 1.0]`
//! (холст MCU 1920×1080 пересчитывается один раз, на сервере), поэтому клиент
//! может нарисовать плитки в любом своём разрешении.
//!
//! **Нумерация слотов:** `Slot.id` в протоколе — 0-based (контракт
//! `LayoutDescriptor`), совпадает с `vks_layout::SlotId`.

use vks_layout::{LayoutKind, NormalizedLayout, NormalizedRect, NormalizedSlot, ParticipantId, SlotId};
use vks_proto::signaling::{Layout, SlotAssignment};

/// Преобразует прото-раскладку в нормализованную модель `vks-layout`.
///
/// Участник в слоте необязателен: пустой слот (окно без участника) —
/// это норма, а не ошибка.
#[must_use]
pub fn normalized_layout_from_proto(proto: &Layout) -> NormalizedLayout {
    let slots = proto
        .slots
        .iter()
        .map(|slot| {
            let rect = slot.rect.as_ref().map_or(NormalizedRect::new(0.0, 0.0, 0.0, 0.0), |rect| {
                NormalizedRect::new(f64::from(rect.x), f64::from(rect.y), f64::from(rect.w), f64::from(rect.h))
            });
            NormalizedSlot {
                id: SlotId::new(slot.id),
                participant_id: slot
                    .participant_id
                    .as_deref()
                    .and_then(|id| uuid::Uuid::parse_str(id).ok())
                    .map(|uuid| ParticipantId::new(uuid.as_u128())),
                rect,
                z_index: slot.z as i32,
                pinned: slot.assignment == SlotAssignment::Pinned as i32,
                is_focus: false,
            }
        })
        .collect();
    NormalizedLayout { version: proto.version, kind: LayoutKind::parse(&proto.kind), slots }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vks_proto::signaling::{Rect, Slot};

    #[test]
    fn converts_slots_and_participants() {
        let proto = Layout {
            version: 4,
            kind: "custom".into(),
            slots: vec![
                Slot {
                    id: 0,
                    rect: Some(Rect { x: 0.0, y: 0.0, w: 0.5, h: 0.5 }),
                    z: 0,
                    assignment: SlotAssignment::Fixed as i32,
                    participant_id: Some("00000000-0000-0000-0000-0000000000aa".into()),
                },
                Slot {
                    id: 1,
                    rect: Some(Rect { x: 0.5, y: 0.0, w: 0.5, h: 1.0 }),
                    z: 2,
                    assignment: SlotAssignment::Pinned as i32,
                    participant_id: None,
                },
            ],
        };
        let layout = normalized_layout_from_proto(&proto);
        assert_eq!(layout.version, 4);
        assert_eq!(layout.kind, LayoutKind::Custom);
        assert_eq!(layout.slots.len(), 2);
        assert_eq!(layout.slots[0].id.as_u32(), 0);
        assert_eq!(layout.slots[1].id.as_u32(), 1);
        assert!((layout.slots[0].rect.x - 0.0).abs() < 1e-6);
        assert!((layout.slots[1].rect.w - 0.5).abs() < 1e-6);
        assert!(layout.slots[0].participant_id.is_some());
        assert!(layout.slots[1].participant_id.is_none());
        assert!(layout.slots[1].pinned, "PINNED → pinned");
        assert_eq!(layout.slots[1].z_index, 2);
    }

    #[test]
    fn scales_to_client_canvas() {
        let proto = Layout {
            version: 1,
            kind: "grid".into(),
            slots: vec![Slot {
                id: 0,
                rect: Some(Rect { x: 0.0, y: 0.0, w: 0.5, h: 0.5 }),
                z: 0,
                assignment: SlotAssignment::Auto as i32,
                participant_id: None,
            }],
        };
        let layout = normalized_layout_from_proto(&proto);
        let canvas = vks_layout::Canvas::new(800, 600);
        let pixels = layout.scale_to_canvas(&canvas);
        assert_eq!(pixels.slots[0].rect.w, 400);
        assert_eq!(pixels.slots[0].rect.h, 300);
    }
}
