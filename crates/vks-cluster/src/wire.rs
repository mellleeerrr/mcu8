//! Стабильный wire-формат кластерного сообщения (для NATS и межпроцессного обмена).
//!
//! Основание: `docs/architecture/CLUSTERING.md` §5 (per-subject ordering,
//! idempotency по `message_id`), ADR-0019, T14.
//!
//! Формат версионирован и **зафиксирован golden-тестом**: любое изменение
//! layout'а обязано поднять [`WIRE_VERSION`], иначе два узла с разными
//! сборками молча разойдутся в трактовке полей.
//!
//! Layout v1 (big-endian):
//!
//! ```text
//! offset  size  field
//! 0       4     magic "VKSM"
//! 4       1     version (1)
//! 5       1     flags: bit0 = critical, остальные = 0
//! 6       1     id_len (36, canonical UUIDv7)
//! 7       id_len  id (ASCII)
//! ...     2     topic_len (u16)
//! ...     topic_len  topic (UTF-8)
//! ...     4     payload_len (u32)
//! ...     payload_len  payload
//! ```
//!
//! Ограничения: topic ≤ [`MAX_TOPIC_LEN`], payload ≤ [`MAX_PAYLOAD_LEN`].
//! Декодер отвергает неизвестную версию, не-UUIDv7 id, мусор после payload
//! и превышение лимитов — «молчаливое» доверие к сети недопустимо.

/// Magic-байты формата.
pub const WIRE_MAGIC: [u8; 4] = *b"VKSM";

/// Текущая версия wire-формата.
pub const WIRE_VERSION: u8 = 1;

/// Максимальная длина темы (байты).
pub const MAX_TOPIC_LEN: usize = 1_024;

/// Максимальный payload (байты). Защита от OOM при чтении чужого сообщения.
pub const MAX_PAYLOAD_LEN: usize = 16 * 1024 * 1024;

/// magic (4) + version (1) + flags (1); id_len идёт следующим полем.
const HEADER_LEN: usize = 6;

/// Ошибка кодирования/декодирования wire-формата.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WireError {
    /// Слишком короткий буфер (обрезано на заголовке/поле).
    #[error("truncated message at offset {0}")]
    Truncated(usize),
    /// Неверный magic.
    #[error("bad magic: expected VKSM")]
    BadMagic,
    /// Неизвестная версия формата.
    #[error("unsupported wire version {0}")]
    UnsupportedVersion(u8),
    /// id не является canonical UUIDv7.
    #[error("invalid UUIDv7 message id {0:?}")]
    InvalidId(String),
    /// Тема пустая/не UTF-8/слишком длинная.
    #[error("invalid topic: {0}")]
    InvalidTopic(String),
    /// Payload превышает лимит.
    #[error("payload too large: {0} > {MAX_PAYLOAD_LEN}")]
    PayloadTooLarge(usize),
    /// Лишние байты после payload.
    #[error("trailing bytes after payload: {0}")]
    TrailingBytes(usize),
}

/// Кодирует сообщение в wire-формат v1.
pub fn encode(msg: &crate::bus::Msg) -> Vec<u8> {
    debug_assert!(msg.id.len() <= u8::MAX as usize);
    debug_assert!(msg.topic.len() <= MAX_TOPIC_LEN);
    debug_assert!(msg.payload.len() <= MAX_PAYLOAD_LEN);

    let mut out = Vec::with_capacity(HEADER_LEN + msg.id.len() + 2 + msg.topic.len() + 4 + msg.payload.len());
    out.extend_from_slice(&WIRE_MAGIC);
    out.push(WIRE_VERSION);
    out.push(u8::from(msg.critical));
    out.push(msg.id.len() as u8);
    out.extend_from_slice(msg.id.as_bytes());
    out.extend_from_slice(&(msg.topic.len() as u16).to_be_bytes());
    out.extend_from_slice(msg.topic.as_bytes());
    out.extend_from_slice(&(msg.payload.len() as u32).to_be_bytes());
    out.extend_from_slice(&msg.payload);
    out
}

fn take<'a>(bytes: &'a [u8], offset: &mut usize, len: usize) -> Result<&'a [u8], WireError> {
    let end = offset.checked_add(len).ok_or(WireError::Truncated(*offset))?;
    let slice = bytes.get(*offset..end).ok_or(WireError::Truncated(*offset))?;
    *offset = end;
    Ok(slice)
}

/// Декодирует сообщение wire-формата v1.
pub fn decode(bytes: &[u8]) -> Result<crate::bus::Msg, WireError> {
    if bytes.len() < HEADER_LEN {
        return Err(WireError::Truncated(0));
    }
    if bytes[0..4] != WIRE_MAGIC {
        return Err(WireError::BadMagic);
    }
    let version = bytes[4];
    if version != WIRE_VERSION {
        return Err(WireError::UnsupportedVersion(version));
    }
    let flags = bytes[5];
    if flags & !0b1 != 0 {
        // Unknown flags would be silently dropped on re-encode → reject instead.
        return Err(WireError::UnsupportedVersion(version));
    }
    let mut offset = HEADER_LEN;

    let id_len = take(bytes, &mut offset, 1)?[0] as usize;
    let id_raw = take(bytes, &mut offset, id_len)?;
    let id = std::str::from_utf8(id_raw).map_err(|_| WireError::InvalidId("<non-UTF-8>".to_string()))?;
    let id = uuid::Uuid::parse_str(id).map_err(|_| WireError::InvalidId(id.to_string()))?;
    if id.get_version_num() != 7 {
        return Err(WireError::InvalidId(id.to_string()));
    }

    let topic_len = u16::from_be_bytes(take(bytes, &mut offset, 2)?.try_into().expect("2 bytes")) as usize;
    if topic_len == 0 || topic_len > MAX_TOPIC_LEN {
        return Err(WireError::InvalidTopic(format!("length {topic_len}")));
    }
    let topic_raw = take(bytes, &mut offset, topic_len)?;
    let topic =
        std::str::from_utf8(topic_raw).map_err(|_| WireError::InvalidTopic("<non-UTF-8>".to_string()))?.to_string();

    let payload_len = u32::from_be_bytes(take(bytes, &mut offset, 4)?.try_into().expect("4 bytes")) as usize;
    if payload_len > MAX_PAYLOAD_LEN {
        return Err(WireError::PayloadTooLarge(payload_len));
    }
    let payload = take(bytes, &mut offset, payload_len)?.to_vec();

    if offset != bytes.len() {
        return Err(WireError::TrailingBytes(bytes.len() - offset));
    }

    Ok(crate::bus::Msg { id: id.to_string(), topic, payload, critical: flags & 0b1 == 1 })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bus::Msg;

    fn fixed_msg() -> Msg {
        Msg {
            id: "0192f0a4-6d7b-7c3e-8a1b-2c3d4e5f6a7b".to_string(),
            topic: "vks.cluster.lease.update".to_string(),
            payload: vec![0, 1, 2, 255],
            critical: true,
        }
    }

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    #[test]
    fn wire_layout_v1_is_frozen() {
        // Golden bytes: if this fails, bump WIRE_VERSION and update both nodes.
        let expected = "564b534d01012430313932663061342d366437622d376333652d386131622d326333643465356636613762\
                        0018766b732e636c75737465722e6c656173652e75706461746500000004000102ff";
        assert_eq!(hex(&encode(&fixed_msg())), expected);
    }

    #[test]
    fn roundtrip_preserves_all_fields() {
        let msg = Msg::new("vks.cluster.heartbeat", vec![9, 8, 7], false);
        let decoded = decode(&encode(&msg)).expect("roundtrip");
        assert_eq!(decoded, msg);
    }

    #[test]
    fn roundtrip_empty_payload_and_critical_flag() {
        let msg = Msg::new("vks.cluster.lease.release", vec![], true);
        let decoded = decode(&encode(&msg)).expect("roundtrip");
        assert_eq!(decoded, msg);
        assert!(decoded.critical);
    }

    #[test]
    fn rejects_bad_magic_version_and_truncation() {
        let bytes = encode(&fixed_msg());
        let mut bad_magic = bytes.clone();
        bad_magic[0] = b'X';
        assert_eq!(decode(&bad_magic), Err(WireError::BadMagic));

        let mut bad_version = bytes.clone();
        bad_version[4] = 99;
        assert_eq!(decode(&bad_version), Err(WireError::UnsupportedVersion(99)));

        for cut in 0..bytes.len() {
            assert!(decode(&bytes[..cut]).is_err(), "truncated at {cut} must be rejected");
        }
    }

    #[test]
    fn rejects_trailing_bytes() {
        let mut bytes = encode(&fixed_msg());
        bytes.push(0);
        assert_eq!(decode(&bytes), Err(WireError::TrailingBytes(1)));
    }

    #[test]
    fn rejects_non_uuid_v7_id() {
        let mut msg = fixed_msg();
        msg.id = "msg-0".to_string();
        // `encode` is intentionally permissive at the type level; decode enforces
        // the invariant that every cluster message carries a UUIDv7 id.
        assert!(matches!(decode(&encode(&msg)), Err(WireError::InvalidId(_))));

        let mut v4 = fixed_msg();
        v4.id = "f47ac10b-58cc-4372-a567-0e02b2c3d479".to_string();
        assert!(matches!(decode(&encode(&v4)), Err(WireError::InvalidId(_))));
    }

    #[test]
    fn rejects_unknown_flags_and_empty_topic() {
        let bytes = encode(&fixed_msg());
        let mut flags = bytes.clone();
        flags[5] = 0b1000_0001;
        assert!(matches!(decode(&flags), Err(WireError::UnsupportedVersion(_))));

        let mut msg = fixed_msg();
        msg.topic.clear();
        assert!(matches!(decode(&encode(&msg)), Err(WireError::InvalidTopic(_))));
    }

    #[test]
    fn rejects_uncompressed_payload_over_limit() {
        // Build a header claiming > MAX_PAYLOAD_LEN without allocating it.
        let mut bytes = encode(&fixed_msg());
        let payload_len_offset = bytes.len() - 4 - 4;
        bytes[payload_len_offset..payload_len_offset + 4]
            .copy_from_slice(&((MAX_PAYLOAD_LEN + 1) as u32).to_be_bytes());
        assert!(matches!(decode(&bytes), Err(WireError::PayloadTooLarge(n)) if n == MAX_PAYLOAD_LEN + 1));
    }

    #[test]
    fn critical_flag_is_the_only_flag_bit() {
        for critical in [false, true] {
            let msg = Msg { critical, ..fixed_msg() };
            assert_eq!(decode(&encode(&msg)).expect("roundtrip").critical, critical);
        }
    }
}
