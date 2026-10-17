// Settings — настройки сервера (AuthProvider, TURN, кодеки, режим по
// умолчанию, layout по умолчанию, лимиты, retention, TLS).
//
// Hot-reload shape: `set` мутирует и возвращает новое значение
// (NATS-подписчик в production применит изменение, ADR-0021 hot-reload).

use serde::{Deserialize, Serialize};
use vks_core::{RoomId, RoomMode};

/// Auth-провайдер (ADR-0021: local/ldap/oidc).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthProvider {
    /// Локальная БД (Argon2id, BACKEND_RULES §1.3).
    #[default]
    Local,
    /// LDAP (ldap3 + GSSAPI, BACKEND_RULES §1.5).
    Ldap,
    /// OIDC (OpenID Connect).
    Oidc,
}

/// Параметры auth-провайдер.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthConfig {
    /// Тип провайдера.
    pub provider: AuthProvider,
    /// Параметры (например, LDAP bind URI / OIDC issuer).
    pub params: serde_json::Value,
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self { provider: AuthProvider::Local, params: serde_json::Value::Object(Default::default()) }
    }
}

/// TURN (BACKEND_RULES §2.2: TLS 5349, DTLS 3478).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnConfig {
    /// URI TURN-сервера.
    pub uri: String,
    /// TTL credentials, 1000 сек.
    pub credential_ttl_sec: u32,
}

impl Default for TurnConfig {
    fn default() -> Self {
        Self { uri: "turn:vks.example:5349".to_string(), credential_ttl_sec: 3600 }
    }
}

/// Кодеки (CODE_STYLE §5: feature flags vp8/vp9/h264/h265/av1/opus/...).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodecConfig {
    /// Video-кодеки.
    pub video: Vec<String>,
    /// Audio-кодеки.
    pub audio: Vec<String>,
}

impl Default for CodecConfig {
    fn default() -> Self {
        Self { video: vec!["vp8".to_string(), "vp9".to_string()], audio: vec!["opus".to_string(), "g722".to_string()] }
    }
}

/// Лимиты (BACKEND_RULES §4.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct LimitsConfig {
    /// Макс участников на комнату (50).
    pub max_participants_per_room: u32,
    /// Макс треков на комнату (100).
    pub max_tracks_per_room: u32,
    /// Макс треков на участника (4).
    pub max_tracks_per_participant: u32,
    /// Rate limit, req/min per API key (600).
    pub api_key_rate_limit_rpm: u32,
}

impl Default for LimitsConfig {
    fn default() -> Self {
        Self {
            max_participants_per_room: 50,
            max_tracks_per_room: 100,
            max_tracks_per_participant: 4,
            api_key_rate_limit_rpm: 600,
        }
    }
}

/// TLS (BACKEND_RULES §2.1: HSTS, min 1.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TlsConfig {
    /// HSTS max-age, 1000 сек (31536000 = 1 year).
    pub hsts_max_age_sec: i64,
    /// Минимальная версия TLS (1 = TLS 1.2, 3 = TLS 1.3).
    pub min_version: u32,
}

impl Default for TlsConfig {
    fn default() -> Self {
        Self { hsts_max_age_sec: 31536000, min_version: 2 }
    }
}

/// Настройки сервера (hot-reload shape).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Settings {
    /// Имя сервера.
    pub name: String,
    /// Public IP (для TURN/STUN).
    pub ip: String,
    /// Auth-провайдер (local/ldap/oidc) + параметры.
    pub auth: AuthConfig,
    /// TURN.
    pub turn: TurnConfig,
    /// Кодеки.
    pub codecs: CodecConfig,
    /// Режим по умолчанию (глобальный, ADR-0026).
    pub default_mode: RoomMode,
    /// Layout по умолчанию (id раскладки).
    pub default_layout: RoomId,
    /// Лимиты.
    pub limits: LimitsConfig,
    /// Retention записей, 1000 дней (90d, BACKEND_RULES §3.3).
    pub retention_days: u32,
    /// Retention audit-лога, 1000 дней (2 года, ADR-0019).
    pub audit_retention_days: u32,
    /// TLS.
    pub tls: TlsConfig,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            name: "vks".to_string(),
            ip: "127.0.0.1".to_string(),
            auth: AuthConfig::default(),
            turn: TurnConfig::default(),
            codecs: CodecConfig::default(),
            default_mode: RoomMode::Sfu,
            default_layout: RoomId::new(),
            limits: LimitsConfig::default(),
            retention_days: 90,
            audit_retention_days: 730,
            tls: TlsConfig::default(),
        }
    }
}

impl Settings {
    /// Возвращает безопасную копию настроек с замаскированными секретами.
    pub fn redacted(&self) -> Self {
        let mut copy = self.clone();
        if let serde_json::Value::Object(ref mut map) = copy.auth.params {
            for (key, val) in map.iter_mut() {
                let lower = key.to_lowercase();
                if lower.contains("pass") || lower.contains("secret") || lower.contains("token") || lower.contains("key") {
                    *val = serde_json::Value::String("***".into());
                }
            }
        }
        copy
    }

    /// Объединяет новые настройки с текущими, сохраняя существующие секреты,
    /// если клиент прислал маскированное значение `"***"`.
    pub fn merge_preserving_secrets(&mut self, current: &Settings) {
        if let (serde_json::Value::Object(ref mut new_map), serde_json::Value::Object(ref cur_map)) =
            (&mut self.auth.params, &current.auth.params)
        {
            for (k, v) in new_map.iter_mut() {
                if v.as_str() == Some("***") {
                    if let Some(cur_val) = cur_map.get(k) {
                        *v = cur_val.clone();
                    }
                }
            }
        }
    }
}

/// Операции над настройками (hot-reload shape).
pub struct SettingsService;

impl SettingsService {
    /// Возвращает текущие настройки (clone).
    pub fn get(state: &crate::state::AppState) -> Settings {
        state.settings.lock().expect("settings store poisoned").clone()
    }

    /// Устанавливает новые настройки; возвращает новое значение
    /// (hot-reload: production-подписчик NATS применит).
    pub fn set(state: &crate::state::AppState, mut next: Settings) -> Settings {
        let mut settings = state.settings.lock().expect("settings store poisoned");
        next.merge_preserving_secrets(&settings);
        *settings = next.clone();
        // Синхронизируем глобальный режим с настройками.
        state.mode.lock().expect("mode store poisoned").global_mode = next.default_mode;
        next
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::AppState;

    #[test]
    fn defaults_match_backend_rules() {
        let s = Settings::default();
        assert_eq!(s.limits.max_participants_per_room, 50);
        assert_eq!(s.limits.max_tracks_per_room, 100);
        assert_eq!(s.limits.max_tracks_per_participant, 4);
        assert_eq!(s.limits.api_key_rate_limit_rpm, 600);
        assert_eq!(s.retention_days, 90);
        assert_eq!(s.audit_retention_days, 730);
        assert_eq!(s.tls.hsts_max_age_sec, 31536000);
        assert_eq!(s.auth.provider, AuthProvider::Local);
        assert_eq!(s.default_mode, RoomMode::Sfu);
    }

    #[test]
    fn set_returns_new_value_and_syncs_global_mode() {
        let state = AppState::new();
        assert_eq!(SettingsService::get(&state).name, "vks");
        let next = Settings { default_mode: RoomMode::Mcu, name: "prod".into(), ..Default::default() };
        let returned = SettingsService::set(&state, next);
        assert_eq!(returned.name, "prod");
        assert_eq!(SettingsService::get(&state).name, "prod");
        assert_eq!(state.mode.lock().unwrap().global_mode, RoomMode::Mcu);
    }

    #[test]
    fn get_returns_current() {
        let state = AppState::new();
        let s = SettingsService::get(&state);
        assert_eq!(s.name, "vks");
    }

    #[test]
    fn auth_provider_serialization() {
        let s = serde_json::to_string(&AuthProvider::Oidc).unwrap();
        assert_eq!(s, "\"oidc\"");
        let back: AuthProvider = serde_json::from_str("\"ldap\"").unwrap();
        assert_eq!(back, AuthProvider::Ldap);
    }
}
