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
        };
        let bytes = layout.encode_to_vec();
        let back = signaling::Layout::decode(&bytes[..]).expect("decode");
        assert_eq!(back.slots.len(), 1);
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
