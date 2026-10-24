//! Рассылка раскладки комнаты клиентам (`LayoutChanged`).
//!
//! До появления этого модуля прототип `SignalingMessage.layout_changed`
//! (поле 80) был объявлен, но **никогда не отправлялся**: сервер применял
//! раскладку в MCU, а клиент продолжал рисовать собственную авто-сетку.
//! Раскладка — единственный источник истины о геометрии плиток, поэтому
//! после каждого `apply_layout` она рассылается участникам комнаты.
//!
//! **Источник истины — эффективная раскладка композитора**
//! (`McuEngine::layout_snapshot`), а не дескриптор: композитор считает
//! авто-сетку сам, клампит прямоугольники в холст и раскладывает участников
//! по слотам. Дескриптор без геометрии (режим auto) дал бы клиенту пустой
//! список слотов — то есть пустой экран.
//!
//! **Единица координат:** `Rect` в протоколе — **нормализованная** (0..1)
//! относительно холста MCU. Клиент рисует плитки в своём размере, поэтому
//! пиксели холста 1920×1080 пересчитываются в доли один раз, на сервере.
//!
//! **Нумерация слотов:** `Slot.id` — всегда 0-based (контракт
//! `LayoutDescriptor`: `assignments`/`geometry[].slot` — 0-based), тот же
//! контракт, что и в `vks-core::ports::media_engine`.

use vks_layout::{Canvas, LayoutKind};
use vks_proto::signaling::signaling_message::Payload;
use vks_proto::signaling::{Layout, LayoutChanged, Rect, SignalingMessage, Slot, SlotAssignment};

/// Собирает прото-раскладку из эффективной раскладки композитора.
///
/// Нормализация — через `vks_layout` (`Layout::to_normalized`), чтобы сервер
/// и клиент считали координаты одним кодом.
#[must_use]
pub fn proto_layout(layout: &vks_layout::Layout, preset: &str, mode: &str) -> Layout {
    let (canvas_w, canvas_h) = vks_media_mcu::CANVAS_SIZE;
    let canvas = Canvas::new(canvas_w, canvas_h);
    let normalized = layout.to_normalized(&canvas);
    let slots = normalized
        .slots
        .iter()
        .map(|slot| {
            let participant_id = slot.participant_id.map(|pid| uuid::Uuid::from_u128(pid.as_u128()).to_string());
            Slot {
                id: slot.id.as_u32(),
                rect: Some(Rect {
                    x: slot.rect.x as f32,
                    y: slot.rect.y as f32,
                    w: slot.rect.w as f32,
                    h: slot.rect.h as f32,
                }),
                z: slot.z_index.max(0) as u32,
                // Закреплённый слот — «pinned», занятый — «fixed», свободный —
                // «auto»: клиент различает их при наведении и в self-view.
                assignment: if slot.pinned {
                    SlotAssignment::Pinned as i32
                } else if participant_id.is_some() {
                    SlotAssignment::Fixed as i32
                } else {
                    SlotAssignment::Auto as i32
                },
                participant_id,
            }
        })
        .collect();
    Layout {
        version: layout.version,
        slots,
        kind: layout.kind.as_str().to_string(),
        preset: preset.to_string(),
        mode: mode.to_string(),
    }
}

/// Сообщение `LayoutChanged` для раскладки комнаты.
#[must_use]
pub fn layout_changed(layout: &vks_layout::Layout, preset: &str, mode: &str) -> SignalingMessage {
    SignalingMessage {
        msg_id: 0,
        reply_to: 0,
        payload: Some(Payload::LayoutChanged(LayoutChanged { layout: Some(proto_layout(layout, preset, mode)) })),
    }
}

/// Вид раскладки по строке протокола (для клиента и тестов).
#[must_use]
pub fn layout_kind(name: &str) -> LayoutKind {
    LayoutKind::parse(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use vks_layout::{Layout, Slot};

    fn layout() -> Layout {
        let mut layout = Layout::new(LayoutKind::Grid);
        layout.version = 3;
        layout.slots.push(Slot::new(0, Some(vks_layout::ParticipantId::new(7)), vks_layout::Rect::new(0, 0, 960, 540)));
        layout.slots.push(Slot::new(
            1,
            Some(vks_layout::ParticipantId::new(9)),
            vks_layout::Rect::new(960, 0, 960, 540),
        ));
        layout
    }

    #[test]
    fn slots_are_zero_based_and_normalized() {
        let proto = proto_layout(&layout(), "codian_07", "manual");
        assert_eq!(proto.version, 3);
        assert_eq!(proto.kind, "grid");
        assert_eq!(proto.slots.len(), 2);
        assert_eq!(proto.preset, "codian_07", "T10a: preset доезжает до провода");
        assert_eq!(proto.mode, "manual", "T10a: mode доезжает до провода");
        assert_eq!(proto.slots[0].id, 0);
        assert_eq!(proto.slots[1].id, 1);
        let rect = proto.slots[1].rect.as_ref().expect("rect");
        assert!((rect.x - 0.5).abs() < 1e-6, "x={}", rect.x);
        assert!(rect.y.abs() < 1e-6, "y={}", rect.y);
        assert!((rect.w - 0.5).abs() < 1e-6, "w={}", rect.w);
        assert!((rect.h - 0.5).abs() < 1e-6, "h={}", rect.h);
    }

    #[test]
    fn participant_is_bound_to_its_slot() {
        let proto = proto_layout(&layout(), "codian_07", "manual");
        let first = proto.slots[0].participant_id.as_deref().expect("participant");
        let second = proto.slots[1].participant_id.as_deref().expect("participant");
        assert_ne!(first, second);
        assert_eq!(proto.slots[0].assignment, SlotAssignment::Fixed as i32);
    }

    #[test]
    fn pinned_slot_is_marked_pinned() {
        let mut layout = layout();
        layout.slots[0].pinned = true;
        let proto = proto_layout(&layout, "", "");
        assert_eq!(proto.slots[0].assignment, SlotAssignment::Pinned as i32);
        assert_eq!(proto.slots[1].assignment, SlotAssignment::Fixed as i32);
    }

    #[test]
    fn message_carries_layout_changed_payload() {
        let message = layout_changed(&layout(), "preset_equal_25", "dynamic");
        match message.payload {
            Some(Payload::LayoutChanged(changed)) => {
                let proto = changed.layout.expect("layout");
                assert_eq!(proto.slots.len(), 2);
                assert_eq!(proto.preset, "preset_equal_25");
                assert_eq!(proto.mode, "dynamic");
            }
            other => panic!("unexpected payload: {other:?}"),
        }
    }
}
