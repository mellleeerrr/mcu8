//! Генерация идентификаторов: UUIDv7 (комнаты, участники) и ULID (сообщения).
//!
//! Основание: `docs/DEVELOPMENT_PLAN.md` Фаза 0 (ids.rs),
//! `docs/architecture/SIGNALING_PROTOCOL.md` (msg_id = ULID, participant_id = UUIDv7).
//!
//! Правила (SIGNALING_PROTOCOL.md):
//! - `participant_id` / `room_id` — **UUIDv7** (time-ordered).
//! - `msg_id` / `client_id` / `join_nonce` — **ULID** (или UUIDv7).

use ulid::Ulid;
use uuid::Uuid;

/// Создаёт новый **UUIDv7** (time-ordered) с текущим временем.
#[must_use]
pub fn uuid_v7() -> Uuid {
    Uuid::now_v7()
}

/// Создаёт новый **ULID** (time-ordered, monotonic-friendly; для msg_id).
#[must_use]
pub fn ulid() -> Ulid {
    Ulid::new()
}

/// UUIDv7 как строка (wire/DB).
#[must_use]
pub fn uuid_v7_string() -> String {
    uuid_v7().to_string()
}

/// ULID как строка.
#[must_use]
pub fn ulid_string() -> String {
    ulid().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use std::str::FromStr;
    use uuid::Version;

    proptest! {
        #[test]
        fn uuid_v7_roundtrip(_s: String) {
            let u = uuid_v7();
            let s = u.to_string();
            let parsed = Uuid::from_str(&s).expect("parse own uuid");
            prop_assume!(parsed == u);
            assert_eq!(parsed.get_version(), Some(Version::SortRand));
        }
    }

    #[test]
    fn uuid_v7_is_v7() {
        assert_eq!(uuid_v7().get_version(), Some(Version::SortRand));
    }

    #[test]
    fn ulid_roundtrip() {
        let u = ulid();
        let s = u.to_string();
        let parsed = Ulid::from_str(&s).expect("parse own ulid");
        assert_eq!(parsed, u);
    }

    #[test]
    fn two_ids_differ() {
        assert_ne!(uuid_v7_string(), uuid_v7_string());
        assert_ne!(ulid_string(), ulid_string());
    }

    /// UUIDv7 — time-ordered: два последовательных id сортируются по времени
    /// (второй строго позже первого) — ключевое свойство для room_id/participant_id.
    #[test]
    fn uuid_v7_is_time_ordered() {
        let first = uuid_v7();
        let second = uuid_v7();
        assert!(second > first, "uuid v7 must be sortable by creation time: {second} should be after {first}");
    }

    /// Два последовательных ULID различаются (время + энтропия).
    #[test]
    fn two_sequential_ulids_differ() {
        let first = ulid();
        let second = ulid();
        assert_ne!(first, second);
        assert_ne!(first.to_string(), second.to_string());
    }

    /// Roundtrip UUIDv7: значение → string → parse (стабильный формат на wire/DB).
    /// `uuid_v7()` и `uuid_v7_string()` — разные вызовы (разное время), поэтому
    /// сравниваем roundtrip одного значения, а не результата двух вызовов.
    #[test]
    fn uuid_v7_string_roundtrip() {
        let u = uuid_v7();
        let s = u.to_string();
        let parsed = Uuid::from_str(&s).expect("parse own uuid string");
        assert_eq!(parsed, u);
        assert_eq!(parsed.to_string(), s, "string form must be stable");
    }

    /// Roundtrip ULID: значение → string → parse (стабильный формат msg_id на wire).
    #[test]
    fn ulid_string_roundtrip() {
        let u = ulid();
        let s = u.to_string();
        let parsed = Ulid::from_str(&s).expect("parse own ulid string");
        assert_eq!(parsed, u);
        assert_eq!(parsed.to_string(), s, "string form must be stable");
    }

    /// `uuid_v7_string()` / `ulid_string()` — helper-функции возвращают
    /// корректный (parse-абельный) формат того же типа.
    #[test]
    fn string_helpers_produce_parseable_ids() {
        let us = uuid_v7_string();
        let parsed = Uuid::from_str(&us).expect("uuid_v7_string parseable");
        assert_eq!(parsed.get_version(), Some(Version::SortRand));
        let ls = ulid_string();
        assert!(Ulid::from_str(&ls).is_ok(), "ulid_string() must produce a valid ULID string");
    }
}
