//! Рассылка раскладки комнаты клиентам (`LayoutChanged`).
//!
//! До появления этого модуля прототип `SignalingMessage.layout_changed`
//! (поле 80) был объявлен, но **никогда не отправлялся**: сервер применял
//! раскладку в MCU, а клиент продолжал рисовать собственную авто-сетку.
//! Раскладка — единственный источник истины о геометрии плиток, поэтому
//! после каждого `apply_layout` она рассылается участникам комнаты.
//!
//! **Единица координат:** `Rect` в протоколе — **нормализованная** (0..1)
//! относительно холста MCU. Клиент рисует плитки в своём размере, поэтому
//! пиксели холста 1920×1080 пересчитываются в доли один раз, на сервере.
//!
//! **Нумерация слотов:** `Slot.id` — всегда 0-based (контракт
//! `LayoutDescriptor`: `assignments`/`geometry[].slot` — 0-based), тот же
//! контракт, что и в `vks-core::ports::media_engine`.

use std::collections::BTreeMap;

use vks_core::ports::media_engine::LayoutDescriptor;
use vks_proto::signaling::signaling_message::Payload;
use vks_proto::signaling::{Layout, LayoutChanged, Rect, SignalingMessage, Slot, SlotAssignment};

/// Вид раскладки для клиента: `grid` | `focus` | `speaker` | `custom`.
fn layout_kind(layout_id: &str) -> &'static str {
    match layout_id {
        "grid" | "focus" | "speaker" => "grid",
        _ => "custom",
    }
}

/// Нормализует пиксельный прямоугольник холста MCU к диапазону 0..1.
fn normalized_rect(x: u32, y: u32, width: u32, height: u32) -> Rect {
    let (canvas_w, canvas_h) = vks_media_mcu::CANVAS_SIZE;
    let canvas_w = (canvas_w as f32).max(1.0);
    let canvas_h = (canvas_h as f32).max(1.0);
    Rect { x: x as f32 / canvas_w, y: y as f32 / canvas_h, w: width as f32 / canvas_w, h: height as f32 / canvas_h }
}

/// Собирает прото-раскладку из дескриптора MCU.
///
/// `assignments` разворачивается в обратную карту «слот → участник»: клиенту
/// нужен именно слот (в нём рисуется участник), а не список соответствий.
#[must_use]
pub fn proto_layout(descriptor: &LayoutDescriptor) -> Layout {
    let mut by_slot: BTreeMap<u32, String> = BTreeMap::new();
    for (participant, slot) in &descriptor.assignments {
        by_slot.insert(*slot, participant.to_string());
    }
    let slots = descriptor
        .geometry
        .iter()
        .map(|cell| {
            let participant = by_slot.get(&cell.slot).cloned();
            Slot {
                id: cell.slot,
                rect: Some(normalized_rect(cell.x, cell.y, cell.width, cell.height)),
                z: cell.z_order,
                // Явно занятый слот — «fixed»: участник закреплён за ячейкой.
                assignment: if participant.is_some() {
                    SlotAssignment::Fixed as i32
                } else {
                    SlotAssignment::Auto as i32
                },
                participant_id: participant,
            }
        })
        .collect();
    Layout { version: descriptor.version, slots, kind: layout_kind(&descriptor.layout_id).to_string() }
}

/// Сообщение `LayoutChanged` для дескриптора раскладки.
#[must_use]
pub fn layout_changed(descriptor: &LayoutDescriptor) -> SignalingMessage {
    SignalingMessage {
        msg_id: 0,
        reply_to: 0,
        payload: Some(Payload::LayoutChanged(LayoutChanged { layout: Some(proto_layout(descriptor)) })),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vks_core::ports::media_engine::LayoutSlot;
    use vks_core::ParticipantId;

    fn descriptor() -> LayoutDescriptor {
        let mut assignments = BTreeMap::new();
        assignments.insert(ParticipantId::from_uuid(uuid::Uuid::from_u128(7)), 0);
        assignments.insert(ParticipantId::from_uuid(uuid::Uuid::from_u128(9)), 1);
        LayoutDescriptor {
            layout_id: "grid".into(),
            version: 3,
            assignments,
            geometry: vec![
                LayoutSlot { slot: 0, x: 0, y: 0, width: 960, height: 540, z_order: 0 },
                LayoutSlot { slot: 1, x: 960, y: 0, width: 960, height: 540, z_order: 0 },
            ],
        }
    }

    #[test]
    fn slots_are_zero_based_and_normalized() {
        let layout = proto_layout(&descriptor());
        assert_eq!(layout.version, 3);
        assert_eq!(layout.kind, "grid");
        assert_eq!(layout.slots.len(), 2);
        assert_eq!(layout.slots[0].id, 0);
        assert_eq!(layout.slots[1].id, 1);
        let rect = layout.slots[1].rect.as_ref().expect("rect");
        assert!((rect.x - 0.5).abs() < 1e-6, "x={}", rect.x);
        assert!((rect.y).abs() < 1e-6, "y={}", rect.y);
        assert!((rect.w - 0.5).abs() < 1e-6, "w={}", rect.w);
        assert!((rect.h - 0.5).abs() < 1e-6, "h={}", rect.h);
    }

    #[test]
    fn participant_is_bound_to_its_slot() {
        let layout = proto_layout(&descriptor());
        let first = layout.slots[0].participant_id.as_deref().expect("participant");
        let second = layout.slots[1].participant_id.as_deref().expect("participant");
        assert_ne!(first, second);
        assert_eq!(layout.slots[0].assignment, SlotAssignment::Fixed as i32);
    }

    #[test]
    fn message_carries_layout_changed_payload() {
        let message = layout_changed(&descriptor());
        match message.payload {
            Some(Payload::LayoutChanged(changed)) => {
                let layout = changed.layout.expect("layout");
                assert_eq!(layout.slots.len(), 2);
            }
            other => panic!("unexpected payload: {other:?}"),
        }
    }
}
