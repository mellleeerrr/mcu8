//! Единый `Error` для всех backend-крейтов VKS.
//!
//! Основание: `docs/rules/BACKEND_RULES.md` §6.4 (Error handling),
//! `docs/PROJECT_RULES.md` §19 (ошибки).
//!
//! Правила:
//! - Production: человекочитаемое сообщение (`Display`), детали — в логах.
//! - Каждый вариант несёт `error_code` (строка) и HTTP-статус (для REST-адаптеров).
//! - Stack trace — только в `debug`-режиме / `?trace=1`.

use std::fmt;

/// HTTP-статус, соответствующий классу ошибки (см. BACKEND_RULES §6.4:
/// 400/401/403/404/409/422/429/500).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HttpStatus {
    BadRequest = 400,
    Unauthorized = 401,
    Forbidden = 403,
    NotFound = 404,
    Conflict = 409,
    UnprocessableEntity = 422,
    TooManyRequests = 429,
    Internal = 500,
}

impl HttpStatus {
    /// Числовое значение кода.
    #[must_use]
    pub const fn as_u16(self) -> u16 {
        self as u16
    }
}

impl fmt::Display for HttpStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_u16())
    }
}

/// Строчный `error_code` (машинно-читаемый, стабильный между версиями).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorCode {
    /// Не удалось разобрать входные данные (протокол, JSON, конфиг).
    InvalidInput,
    /// Аутентификация не пройдена (нет/неверный токен).
    AuthFailed,
    /// Авторизация не пройдена (нет прав на действие).
    Forbidden,
    /// Объект не найден (комната, пользователь, запись, layout).
    NotFound,
    /// Конфликт состояния (двойной join, уже занят).
    Conflict,
    /// Semantically неверный запрос (валиден формально, но неверен по смыслу).
    Unprocessable,
    /// Превышен лимит (rate limit, размер, количество).
    RateLimited,
    /// Ошибка внутреннего состояния / unexpected panic-граница.
    Internal,
    /// Медиа-ошибка (codec, ICE, DTLS, BWE).
    Media,
    /// Кластерная шина / ownership.
    Cluster,
}

impl ErrorCode {
    /// Строчный идентификатор (для протокола и логов).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidInput => "invalid_input",
            Self::AuthFailed => "auth_failed",
            Self::Forbidden => "forbidden",
            Self::NotFound => "not_found",
            Self::Conflict => "conflict",
            Self::Unprocessable => "unprocessable",
            Self::RateLimited => "rate_limited",
            Self::Internal => "internal",
            Self::Media => "media",
            Self::Cluster => "cluster",
        }
    }

    /// HTTP-статус, соответствующий коду (см. BACKEND_RULES §6.4).
    #[must_use]
    pub const fn status(self) -> HttpStatus {
        match self {
            Self::InvalidInput => HttpStatus::BadRequest,
            Self::AuthFailed => HttpStatus::Unauthorized,
            Self::Forbidden => HttpStatus::Forbidden,
            Self::NotFound => HttpStatus::NotFound,
            Self::Conflict => HttpStatus::Conflict,
            Self::Unprocessable => HttpStatus::UnprocessableEntity,
            Self::RateLimited => HttpStatus::TooManyRequests,
            Self::Internal => HttpStatus::Internal,
            // Медиа и кластер — внутренние состояния, наружу как 5xx.
            Self::Media | Self::Cluster => HttpStatus::Internal,
        }
    }
}

/// Маркер терминального отказа удалённого терминала (busy/decline/release).
///
/// Живёт в строке ошибки только до границы шлюза: `GatewayManager` снимает
/// префикс перед записью `last_error` и возвратом наружу. Классификация по
/// точному префиксу, а не по подстрокам, исключает ложный `READY` для
/// `sidecar not found`, таймаута на `sip:486...` или IO-ошибки с портом 4040.
pub const TERMINAL_FAILURE_PREFIX: &str = "terminal-reject: ";

/// Помечает сообщение как терминальный отказ терминала.
#[must_use]
pub fn terminal_failure(message: impl std::fmt::Display) -> String {
    format!("{TERMINAL_FAILURE_PREFIX}{message}")
}

/// Является ли сообщение терминальным отказом (точный префикс).
#[must_use]
pub fn is_terminal_failure(message: &str) -> bool {
    // Точный служебный sentinel может оказаться после контекста
    // (`internal error: H.323: ...`), поэтому ищем только его, а не
    // протокольные подстроки вроде `486`/`404`/`not found`.
    message.contains(TERMINAL_FAILURE_PREFIX)
}

/// Снимает служебный префикс для `last_error`/UI.
#[must_use]
pub fn strip_terminal_failure(message: &str) -> &str {
    message.find(TERMINAL_FAILURE_PREFIX).map_or(message, |index| &message[index + TERMINAL_FAILURE_PREFIX.len()..])
}

/// Единый `Error` для backend-крейтов VKS.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Входные данные (протокол/JSON/конфиг) не распознаны.
    #[error("invalid input: {0}")]
    InvalidInput(String),
    /// Аутентификация.
    #[error("authentication failed: {0}")]
    AuthFailed(String),
    /// Авторизация.
    #[error("forbidden: {0}")]
    Forbidden(String),
    /// Объект не найден.
    #[error("not found: {0}")]
    NotFound(String),
    /// Конфликт состояния.
    #[error("conflict: {0}")]
    Conflict(String),
    /// Semantically неверный запрос.
    #[error("unprocessable: {0}")]
    Unprocessable(String),
    /// Лимит превышен.
    #[error("rate limited: {0}")]
    RateLimited(String),
    /// Медиа-ошибка.
    #[error("media error: {0}")]
    Media(String),
    /// Кластерная ошибка.
    #[error("cluster error: {0}")]
    Cluster(String),
    /// Внутренняя / unexpected.
    #[error("internal error: {0}")]
    Internal(#[from] anyhow::Error),
}

impl Error {
    /// Машинно-читаемый код ошибки.
    #[must_use]
    pub const fn code(&self) -> ErrorCode {
        match self {
            Self::InvalidInput(_) => ErrorCode::InvalidInput,
            Self::AuthFailed(_) => ErrorCode::AuthFailed,
            Self::Forbidden(_) => ErrorCode::Forbidden,
            Self::NotFound(_) => ErrorCode::NotFound,
            Self::Conflict(_) => ErrorCode::Conflict,
            Self::Unprocessable(_) => ErrorCode::Unprocessable,
            Self::RateLimited(_) => ErrorCode::RateLimited,
            Self::Media(_) => ErrorCode::Media,
            Self::Cluster(_) => ErrorCode::Cluster,
            Self::Internal(_) => ErrorCode::Internal,
        }
    }

    /// HTTP-статус для REST-адаптера.
    #[must_use]
    pub const fn status(&self) -> HttpStatus {
        self.code().status()
    }
}

// `anyhow::Error` нужен для варианта Internal; подключаем anyhow как workspace-зависимость.
// (Если anyhow не в [dependencies] крейта, добавляется на уровне workspace — см. Cargo.toml.)

#[cfg(test)]
mod tests {
    use super::*;

    /// Полный перебор: каждый `ErrorCode` → ожидаемый HTTP-статус и строковый код.
    /// Основа: `docs/rules/BACKEND_RULES.md` §6.4 (400/401/403/404/409/422/429/500 + error_code).
    #[test]
    fn every_code_maps_to_expected_status_and_string() {
        // (code, ожидаемый статус, ожидаемый error_code)
        let expected: &[(ErrorCode, HttpStatus, &str)] = &[
            (ErrorCode::InvalidInput, HttpStatus::BadRequest, "invalid_input"),
            (ErrorCode::AuthFailed, HttpStatus::Unauthorized, "auth_failed"),
            (ErrorCode::Forbidden, HttpStatus::Forbidden, "forbidden"),
            (ErrorCode::NotFound, HttpStatus::NotFound, "not_found"),
            (ErrorCode::Conflict, HttpStatus::Conflict, "conflict"),
            (ErrorCode::Unprocessable, HttpStatus::UnprocessableEntity, "unprocessable"),
            (ErrorCode::RateLimited, HttpStatus::TooManyRequests, "rate_limited"),
            (ErrorCode::Internal, HttpStatus::Internal, "internal"),
            (ErrorCode::Media, HttpStatus::Internal, "media"),
            (ErrorCode::Cluster, HttpStatus::Internal, "cluster"),
        ];
        for (code, status, str_) in expected {
            assert_eq!(code.status(), *status, "status mismatch for {code:?}");
            assert_eq!(
                (*status).as_u16(),
                match status {
                    HttpStatus::BadRequest => 400,
                    HttpStatus::Unauthorized => 401,
                    HttpStatus::Forbidden => 403,
                    HttpStatus::NotFound => 404,
                    HttpStatus::Conflict => 409,
                    HttpStatus::UnprocessableEntity => 422,
                    HttpStatus::TooManyRequests => 429,
                    HttpStatus::Internal => 500,
                },
                "numeric status mismatch for {status:?}"
            );
            assert_eq!(code.as_str(), *str_, "error_code string mismatch for {code:?}");
        }
    }

    /// `Error`-варианты → `code()` и `status()` (REST-адаптер опирается на оба).
    #[test]
    fn error_variants_expose_code_and_status() {
        let cases: &[(Error, ErrorCode, u16)] = &[
            (Error::InvalidInput("x".into()), ErrorCode::InvalidInput, 400),
            (Error::AuthFailed("x".into()), ErrorCode::AuthFailed, 401),
            (Error::Forbidden("x".into()), ErrorCode::Forbidden, 403),
            (Error::NotFound("x".into()), ErrorCode::NotFound, 404),
            (Error::Conflict("x".into()), ErrorCode::Conflict, 409),
            (Error::Unprocessable("x".into()), ErrorCode::Unprocessable, 422),
            (Error::RateLimited("x".into()), ErrorCode::RateLimited, 429),
            (Error::Media("x".into()), ErrorCode::Media, 500),
            (Error::Cluster("x".into()), ErrorCode::Cluster, 500),
            (Error::Internal(anyhow::anyhow!("boom")), ErrorCode::Internal, 500),
        ];
        for (e, code, status) in cases {
            assert_eq!(e.code(), *code, "code mismatch for {e:?}");
            assert_eq!(e.status().as_u16(), *status, "status mismatch for {e:?}");
        }
    }

    #[test]
    fn code_maps_to_status() {
        let e = Error::NotFound("room".into());
        assert_eq!(e.code(), ErrorCode::NotFound);
        assert_eq!(e.status(), HttpStatus::NotFound);
        assert_eq!(e.status().as_u16(), 404);
    }

    #[test]
    fn terminal_marker_is_exact_and_stripped_even_after_context() {
        let marked = terminal_failure("remote_release:cause=17");
        assert!(is_terminal_failure(&marked));
        assert!(is_terminal_failure(&format!("internal error: H.323: {marked}")));
        assert_eq!(strip_terminal_failure(&marked), "remote_release:cause=17");
        for transport in [
            "H.323 sidecar not found; port 4040",
            "Dial timeout from sip:4861234@host",
            "internal error: H.323: remote_release",
        ] {
            assert!(!is_terminal_failure(transport), "{transport}");
        }
    }

    #[test]
    fn rate_limited_is_429() {
        let e = Error::RateLimited("too many".into());
        assert_eq!(e.status().as_u16(), 429);
        assert_eq!(e.code().as_str(), "rate_limited");
    }

    #[test]
    fn display_is_human_readable() {
        let e = Error::Conflict("room busy".into());
        assert_eq!(e.to_string(), "conflict: room busy");
    }

    #[test]
    fn internal_error_from_anyhow() {
        let e: Error = anyhow::anyhow!("db down").into();
        assert!(matches!(e, Error::Internal(_)));
        assert_eq!(e.code(), ErrorCode::Internal);
        assert_eq!(e.status().as_u16(), 500);
    }
}
