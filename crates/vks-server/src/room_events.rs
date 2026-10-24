//! Доставка событий MCU-движка в сигналинг (S12, ADR-0029 фаза 3).
//!
//! Движок не знает про сигналинг: он отдаёт [`vks_media_mcu::RoomEvent`]
//! приёмнику, который ставит сервер. Здесь события превращаются в сообщения
//! протокола:
//!
//! * `SpeakingStateChanged` (поле 102) — клиент рисует индикатор речи, а
//!   ростер админки обновляет `is_speaking` (раньше VAD оставался внутри
//!   движка и наружу не выходил);
//! * `LayoutChanged` — главное окно переехало к новому спикеру или к
//!   «Важному» участнику (Codian §2.2), клиенту нужна новая геометрия.
//!
//! Оба события — **персональные для комнаты**, поэтому рассылаются через
//! `RoomHub::broadcast` без отправителя: это не эхо участника.

use std::sync::{Arc, Weak};

use vks_admin::state::AppState;
use vks_media_mcu::{EventSink, McuEngine, RoomEvent};
use vks_proto::signaling::signaling_message::Payload;
use vks_proto::signaling::{SignalingMessage, SpeakingStateChanged};

/// Ставит приёмник событий на движок. Повторный вызов заменяет приёмник.
///
/// Держит `Weak`-ссылки: приёмник живёт в движке, а движок — в роутере, поэтому
/// сильные ссылки замкнули бы цикл «состояние → движок → состояние».
pub fn install(state: &Arc<AppState>, mcu: &Arc<McuEngine>) {
    let state: Weak<AppState> = Arc::downgrade(state);
    let engine: Weak<McuEngine> = Arc::downgrade(mcu);
    let sink: EventSink = Arc::new(move |room, event| {
        let (Some(state), Some(engine)) = (state.upgrade(), engine.upgrade()) else {
            return;
        };
        match event {
            RoomEvent::Speaking { participant, speaking, level } => {
                let participant_id = uuid::Uuid::from_u128(participant.as_uuid().as_u128()).to_string();
                // Ростер — источник истины для `GET /participants` и админ-WS.
                state.room_hub.update_participant_speaking(&room, &participant_id, speaking);
                let message = SignalingMessage {
                    msg_id: 0,
                    reply_to: 0,
                    payload: Some(Payload::SpeakingStateChanged(SpeakingStateChanged {
                        participant_id,
                        speaking,
                        level,
                    })),
                };
                let payload = vks_signaling::envelope::Envelope::new(message).encode();
                tracing::trace!(%room, speaking, level, "SpeakingStateChanged разослан");
                state.room_hub.broadcast(&room, None, payload.into());
            }
            RoomEvent::LayoutChanged { version } => {
                let Some(snapshot) = engine.layout_snapshot(room) else {
                    return;
                };
                debug_assert_eq!(snapshot.layout.version, version, "событие пересадки и раскладка разошлись");
                let message = crate::layout_signal::layout_changed(&snapshot.layout, &snapshot.preset, &snapshot.mode);
                let payload = vks_signaling::envelope::Envelope::new(message).encode();
                tracing::debug!(%room, version, "раскладка перестроена голосовой активацией");
                state.room_hub.broadcast(&room, None, payload.into());
            }
        }
    });
    mcu.set_event_sink(sink);
}

#[cfg(test)]
mod tests {
    use super::*;
    use vks_core::ports::media_engine::{LayoutDescriptor, MediaEngine, RoomMediaCfg};
    use vks_core::track::{Track, TrackKind};
    use vks_core::{ParticipantId, RoomMode};
    use vks_proto::signaling::signaling_message::Payload as ReplyPayload;
    use vks_signaling::envelope::Envelope;

    fn room() -> vks_core::RoomId {
        vks_core::RoomId::from_uuid("60000000-0000-0000-0000-000000000001".parse().unwrap())
    }

    fn pid(n: u128) -> ParticipantId {
        ParticipantId::from_uuid(uuid::Uuid::from_u128(n))
    }

    fn video_track(owner: ParticipantId) -> Track {
        Track::new(owner, TrackKind::Video)
    }

    fn snapshot(n: u128) -> vks_admin::hub::ParticipantSnapshot {
        vks_admin::hub::ParticipantSnapshot {
            participant_id: uuid::Uuid::from_u128(n).to_string(),
            display_name: format!("user-{n}"),
            is_recorder: false,
            is_speaking: false,
            track_ids: vec![format!("video-{n}")],
            is_audio_muted: false,
            is_video_muted: false,
            connected_at: 0,
        }
    }

    /// Забирает одно сообщение нужного вида из шины комнаты.
    fn drain(mut rx: tokio::sync::broadcast::Receiver<vks_admin::hub::RoomBroadcastEvent>) -> Vec<SignalingMessage> {
        let mut out = Vec::new();
        while let Ok(event) = rx.try_recv() {
            if let Ok(envelope) = Envelope::decode(&event.payload) {
                out.push(envelope.inner().clone());
            }
        }
        out
    }

    /// S12: VAD движка доходит до клиентов как `SpeakingStateChanged`, а
    /// голосовая активация — как `LayoutChanged` с главным окном у спикера.
    #[tokio::test]
    async fn engine_events_reach_the_signaling_bus() {
        let state = Arc::new(AppState::new());
        let mcu = Arc::new(McuEngine::new());
        install(&state, &mcu);
        let rid = room();
        let a = pid(1);
        let b = pid(2);
        mcu.create_room(RoomMediaCfg { room_id: rid, mode: RoomMode::Mcu, caps: Default::default() }).await.unwrap();
        mcu.add_publisher(rid, a, vec![video_track(a)]).await.unwrap();
        mcu.add_publisher(rid, b, vec![video_track(b)]).await.unwrap();
        mcu.apply_layout(
            rid,
            LayoutDescriptor {
                layout_id: "focus".into(),
                version: 1,
                assignments: [(a, 0u32), (b, 1u32)].into_iter().collect(),
                geometry: Vec::new(),
                preset: "codian_07".into(),
                mode: "manual".into(),
            },
        )
        .await
        .unwrap();
        let (rx, _) = state.room_hub.register_participant(&rid, snapshot(1));
        state.room_hub.register_participant(&rid, snapshot(2));

        // B говорит: 500 мс дебаунса VAD ⇒ состояние фиксируется.
        mcu.set_audio(rid, b, vec![0.9f32; 960]);
        for step in 0..30 {
            mcu.tick_audio_at(rid, 100 + step * 20);
        }

        let messages = drain(rx);
        let speaking = messages
            .iter()
            .find_map(|message| match &message.payload {
                Some(ReplyPayload::SpeakingStateChanged(event)) => Some(event.clone()),
                _ => None,
            })
            .expect("SpeakingStateChanged должен уйти клиентам");
        assert_eq!(speaking.participant_id, uuid::Uuid::from_u128(2).to_string());
        assert!(speaking.speaking, "B начал говорить");
        assert!(speaking.level > 0, "уровень речи не нулевой");

        let layout = messages
            .iter()
            .find_map(|message| match &message.payload {
                Some(ReplyPayload::LayoutChanged(event)) => event.layout.as_ref(),
                _ => None,
            })
            .expect("LayoutChanged должен уйти клиентам");
        assert_eq!(layout.mode, "manual", "VAD-пересадка не теряет режим раскладки");
        assert_eq!(layout.preset, "codian_07", "VAD-пересадка не теряет preset раскладки");
        // Площадь в тысячных долях: приведение 0.75 к целому дало бы 0 и
        // уравняло главное окно с полосой миниатюр.
        let main = layout.slots.iter().max_by_key(|slot| {
            let rect = slot.rect.as_ref();
            u64::from(rect.map_or(0, |r| ((r.w * r.h) * 1000.0).round() as u32))
        });
        let main = main.expect("главное окно");
        assert_eq!(
            main.participant_id.as_deref(),
            Some(uuid::Uuid::from_u128(2).to_string().as_str()),
            "главное окно — у активного спикера"
        );

        // Ростер админки обновился тем же событием.
        let roster = state.room_hub.list_participants(&rid);
        assert!(roster.iter().any(|p| p.participant_id == uuid::Uuid::from_u128(2).to_string() && p.is_speaking));
    }
}
