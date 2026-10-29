//! `vks-proto` — проводной протокол VKS (protobuf/prost).
//!
//! Генерируется из `proto/*.proto` (signaling, media, cluster, admin).
//! Крейт-привязка: `docs/architecture/SIGNALING_PROTOCOL.md`.
//!
//! prost генерирует **плоские** файлы по protobuf-пакетам
//! (`vks.signaling.rs`, `vks.media.rs`, ...). Они ссылаются друг на друга
//! через `super::`, поэтому объединяются под единым родителем `vks`.
//!
//! Идентификаторы: prost генерирует camelCase-структуры и snake_case-варианты
//! enum (`RoomMode::Mcu`), oneof-контейнеры вложенные
//! (`signaling_message::Payload`).
//!
//! Дерево зависимостей: standalone (ARCHITECTURE.md §3).

#[rustfmt::skip]
mod vks {
    pub mod signaling {
        include!(concat!(env!("OUT_DIR"), "/vks.signaling.rs"));
    }
    pub mod media {
        include!(concat!(env!("OUT_DIR"), "/vks.media.rs"));
    }
    pub mod cluster {
        include!(concat!(env!("OUT_DIR"), "/vks.cluster.rs"));
    }
    pub mod admin {
        include!(concat!(env!("OUT_DIR"), "/vks.admin.rs"));
    }
}

pub use vks::{admin, cluster, media, signaling};

#[cfg(test)]
mod tests {
    use super::*;
    use prost::Message;

    /// T08: `JoinRoomResult.room_mode = 7` is additive. An old server that
    /// does not send it decodes as `Unspecified` (proto3 default) and an old
    /// client ignores it; a new server's MCU value must survive the roundtrip.
    #[test]
    fn join_room_result_room_mode_is_an_additive_wire_field() {
        let mut message = signaling::SignalingMessage {
            msg_id: 7,
            reply_to: 0,
            payload: Some(signaling::signaling_message::Payload::JoinRoomResult(signaling::JoinRoomResult {
                room_mode: media::RoomMode::Mcu as i32,
                ..Default::default()
            })),
        };
        let bytes = message.encode_to_vec();
        match signaling::SignalingMessage::decode(&bytes[..]).expect("decode").payload {
            Some(signaling::signaling_message::Payload::JoinRoomResult(result)) => {
                assert_eq!(result.room_mode, media::RoomMode::Mcu as i32);
            }
            other => panic!("expected JoinRoomResult, got {other:?}"),
        }

        // Old frame (field absent) → proto3 default 0, clients keep their pipeline.
        message = signaling::SignalingMessage {
            msg_id: 8,
            reply_to: 0,
            payload: Some(signaling::signaling_message::Payload::JoinRoomResult(signaling::JoinRoomResult::default())),
        };
        match signaling::SignalingMessage::decode(&message.encode_to_vec()[..]).expect("decode").payload {
            Some(signaling::signaling_message::Payload::JoinRoomResult(result)) => {
                assert_eq!(result.room_mode, media::RoomMode::Unspecified as i32);
            }
            other => panic!("expected JoinRoomResult, got {other:?}"),
        }
    }

    // Roundtrip: SignalingMessage с JoinRoom → bytes → обратно.
    #[test]
    fn signaling_join_room_roundtrip() {
        let msg = signaling::SignalingMessage {
            msg_id: 42,
            reply_to: 0,
            payload: Some(signaling::signaling_message::Payload::JoinRoom(signaling::JoinRoom {
                room_id: "room-1".into(),
                display_name: "Alice".into(),
                is_recorder: false,
            })),
        };

        let bytes = msg.encode_to_vec();
        let back = signaling::SignalingMessage::decode(&bytes[..]).expect("decode");

        assert_eq!(back.msg_id, 42);
        match back.payload {
            Some(signaling::signaling_message::Payload::JoinRoom(j)) => {
                assert_eq!(j.room_id, "room-1");
                assert_eq!(j.display_name, "Alice");
            }
            other => panic!("expected JoinRoom, got {other:?}"),
        }
    }

    // Roundtrip: ServerHello с RoomMode + caps.
    #[test]
    fn server_hello_roundtrip() {
        let hello = signaling::ServerHello {
            proto_version: 1,
            min_supported: 1,
            max_supported: 3,
            server_id: "srv-1".into(),
            server_name: String::new(),
            room_mode: media::RoomMode::Mcu as i32,
            caps: Some(media::RoomCaps {
                room_mode: media::RoomMode::Mcu as i32,
                video_codecs: vec![media::CodecId::H264 as i32],
                audio_codecs: Vec::new(),
                simulcast: false,
                svc: false,
                dtls_srtp: true,
            }),
            ice: None,
        };
        let bytes = hello.encode_to_vec();
        let back = signaling::ServerHello::decode(&bytes[..]).expect("decode");
        assert_eq!(back.max_supported, 3);
        assert_eq!(back.room_mode, media::RoomMode::Mcu as i32);
    }

    // Layout roundtrip.
    #[test]
    fn layout_roundtrip() {
        let layout = signaling::Layout {
            version: 1,
            slots: vec![signaling::Slot {
                id: 1,
                rect: Some(signaling::Rect { x: 0.0, y: 0.0, w: 1.0, h: 1.0 }),
                z: 0,
                assignment: signaling::SlotAssignment::Auto as i32,
                participant_id: None,
            }],
            kind: "grid".into(),
            preset: "codian_07".into(),
            mode: "manual".into(),
        };
        let bytes = layout.encode_to_vec();
        let back = signaling::Layout::decode(&bytes[..]).expect("decode");
        assert_eq!(back.slots.len(), 1);
        assert_eq!(back.kind, "grid");
        assert_eq!(back.preset, "codian_07", "T10a: preset админ-состояния на проводе");
        assert_eq!(back.mode, "manual", "T10a: режим редактора на проводе");
    }

    /// T10a: теги 4/5 не переиспользуются (аддитивная эволюция): кодируем
    /// минимальный Layout с preset/mode и пиним байты.
    #[test]
    fn layout_metadata_wire_tags_are_stable() {
        let layout = signaling::Layout {
            version: 1,
            slots: Vec::new(),
            kind: "grid".into(),
            preset: "x".into(),
            mode: "auto".into(),
        };
        let bytes = layout.encode_to_vec();
        assert_eq!(
            bytes,
            vec![0x08, 0x01, 0x1a, 0x04, b'g', b'r', b'i', b'd', 0x22, 0x01, b'x', 0x2a, 0x04, b'a', b'u', b't', b'o'],
            "preset=4 (0x22), mode=5 (0x2a)"
        );
    }

    /// T10a: additive-эволюция — старый кадр без `preset/mode` декодируется
    /// ровно как раньше, неизвестные поля = "" (не `auto`).
    #[test]
    fn legacy_layout_without_metadata_decodes_to_empty_strings() {
        let layout = signaling::Layout { version: 2, slots: Vec::new(), kind: "grid".into(), ..Default::default() };
        let bytes = layout.encode_to_vec();
        let back = signaling::Layout::decode(&bytes[..]).expect("decode");
        assert_eq!(back.preset, "");
        assert_eq!(back.mode, "");
        assert_eq!(back.kind, "grid");
    }

    // Cluster roundtrip.
    #[test]
    fn cluster_ownership_roundtrip() {
        let msg = cluster::ClusterMessage {
            message_id: 7,
            payload: Some(cluster::cluster_message::Payload::RoomOwnership(cluster::RoomOwnership {
                room_id: "r".into(),
                owner_instance_id: "i".into(),
                lease_ttl_ms: 0,
                epoch: 0,
            })),
        };
        let bytes = msg.encode_to_vec();
        let back = cluster::ClusterMessage::decode(&bytes[..]).expect("decode");
        assert_eq!(back.message_id, 7);
    }

    // Admin DTO roundtrip.
    #[test]
    fn admin_room_dto_roundtrip() {
        let dto = admin::RoomDto {
            id: "x".into(),
            name: "n".into(),
            mode: media::RoomMode::Sfu as i32,
            active: false,
            participant_count: 0,
            created_at_ms: 0,
        };
        let bytes = dto.encode_to_vec();
        let back = admin::RoomDto::decode(&bytes[..]).expect("decode");
        assert_eq!(back.id, "x");
    }
}
