//! Кодек сигнального envelope поверх `vks-proto::SignalingMessage`.
//!
//! Основание: `docs/architecture/SIGNALING_PROTOCOL.md` §1 (Envelope).
//! Каждый WS/QUIC-фрейм = один байтовое сообщение `SignalingMessage`
//! (protobuf). Обёртка даёт: encode/decode + извлечение `msg_id`/`reply_to`
//! и payload для роутинга в handler.

use prost::Message;
use vks_proto::signaling::{self, signaling_message::Payload, SignalingMessage};

/// Обёртка над `SignalingMessage`: сериализация + доступ к полям.
pub struct Envelope {
    msg: SignalingMessage,
}

impl Envelope {
    /// Создаёт envelope из готового `SignalingMessage`.
    #[must_use]
    pub fn new(msg: SignalingMessage) -> Self {
        Self { msg }
    }

    /// id сообщения (msg_id, ULID как u64).
    #[must_use]
    pub fn msg_id(&self) -> u64 {
        self.msg.msg_id
    }

    /// id запроса, на который это ответ (reply_to); 0 = push.
    #[must_use]
    pub fn reply_to(&self) -> u64 {
        self.msg.reply_to
    }

    /// `true`, если это ответ (reply_to != 0), `false` — запрос/push.
    #[must_use]
    pub fn is_reply(&self) -> bool {
        self.msg.reply_to != 0
    }

    /// Доступ к payload (для роутинга в handler).
    #[must_use]
    pub fn payload(&self) -> &Option<Payload> {
        &self.msg.payload
    }

    /// Мутабельный доступ к payload (для обогащения снимками ростера).
    #[must_use]
    pub fn payload_mut(&mut self) -> &mut Option<Payload> {
        &mut self.msg.payload
    }

    /// Внутренний `SignalingMessage`.
    #[must_use]
    pub fn inner(&self) -> &SignalingMessage {
        &self.msg
    }

    /// Извлекает внутренний `SignalingMessage`.
    #[must_use]
    pub fn into_inner(self) -> SignalingMessage {
        self.msg
    }

    /// Сериализует в protobuf-байты (один фрейм).
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        self.msg.encode_to_vec()
    }

    /// Десериализует из байтов (один фрейм).
    pub fn decode(bytes: &[u8]) -> Result<Self, prost::DecodeError> {
        let msg = SignalingMessage::decode(bytes)?;
        Ok(Self { msg })
    }

    /// Строит `Error`-сообщение (reply на запрос) по code/message/retryable.
    #[must_use]
    pub fn error_reply(reply_to: u64, code: &str, message: &str, retryable: bool) -> Self {
        Self::new(SignalingMessage {
            msg_id: 0,
            reply_to,
            payload: Some(Payload::Error(signaling::Error { code: code.into(), message: message.into(), retryable })),
        })
    }

    /// Строит `Ack` (reply на запрос, подтверждение получения).
    #[must_use]
    pub fn ack_reply(reply_to: u64) -> Self {
        Self::new(SignalingMessage {
            msg_id: 0,
            reply_to,
            payload: Some(Payload::Ack(signaling::Ack { acked_msg_id: reply_to.to_string() })),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn join_envelope() -> Envelope {
        Envelope::new(SignalingMessage {
            msg_id: 7,
            reply_to: 0,
            payload: Some(Payload::JoinRoom(signaling::JoinRoom {
                room_id: "r1".into(),
                display_name: "a".into(),
                is_recorder: false,
            })),
        })
    }

    #[test]
    fn roundtrip_encode_decode() {
        let e = join_envelope();
        let bytes = e.encode();
        let back = Envelope::decode(&bytes).expect("decode");
        assert_eq!(back.msg_id(), 7);
        assert!(!back.is_reply());
        assert!(matches!(back.payload(), Some(Payload::JoinRoom(_))));
    }

    #[test]
    fn reply_flags() {
        let err = Envelope::error_reply(42, "auth_failed", "bad token", false);
        assert!(err.is_reply());
        assert_eq!(err.reply_to(), 42);
        let ack = Envelope::ack_reply(99);
        assert!(ack.is_reply());
        assert_eq!(ack.reply_to(), 99);
    }

    #[test]
    fn bad_bytes_error() {
        assert!(Envelope::decode(&[0xFF, 0xFF, 0xFF, 0xFF, 0xFF]).is_err());
    }
}
