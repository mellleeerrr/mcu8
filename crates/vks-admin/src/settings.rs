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
        // Матрица из эталона TrueConf MCU 2.2.1.66 + требования заказчика:
        // G.719 и H.261/H.265, которых в эталонном списке не было, добавлены
        // осознанно. Audio хранится строкой `NAME[:bitrate]`, чтобы PATCH
        // сохранял битрейт варианта G.722.1/C и G.726.
        Self {
            video: ["H261", "H263", "H263+", "H264", "H264-HIGH", "H264-SVC", "H265", "VP8"]
                .into_iter()
                .map(str::to_string)
                .collect(),
            audio: [
                "AAC",
                "G711",
                "G719",
                "G722",
                "G7221:24",
                "G7221:32",
                "G7221C:24",
                "G7221C:32",
                "G7221C:48",
                "G723",
                "G726:16",
                "G726:24",
                "G726:32",
                "G726:40",
                "G728",
                "G729",
                "OPUS",
                "ILBC",
            ]
            .into_iter()
            .map(str::to_string)
            .collect(),
        }
    }
}

impl CodecConfig {
    /// Ранее сохранённые настройки содержали урезанный список `vp8/vp9` +
    /// `opus/g722`. При гидрации из БД такой список нужно расширить до
    /// эталонной матрицы, иначе UI и SIP не увидят требуемые кодеки.
    pub fn migrate_legacy(&mut self) {
        let legacy_video = self.video.iter().map(String::as_str).eq(["vp8", "vp9"]);
        let legacy_audio = self.audio.iter().map(|spec| Self::audio_parts(spec).0).eq(["opus", "g722"]);
        if legacy_video && legacy_audio {
            *self = Self::default();
        }
    }

    /// Разбирает `G7221:24` в `("G7221", Some(24))`; без суффикса —
    /// `("AAC", None)`. Формат терпит и `G7221/24`, и регистр.
    pub fn audio_parts(spec: &str) -> (String, Option<u32>) {
        let spec = spec.trim();
        let (name, bitrate) = spec
            .split_once(':')
            .or_else(|| spec.split_once('/'))
            .map_or((spec, None), |(name, bitrate)| (name, bitrate.trim().parse().ok()));
        (name.trim().to_string(), bitrate)
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

fn redact_json_secrets(val: &mut serde_json::Value) {
    match val {
        serde_json::Value::Object(map) => {
            for (key, v) in map.iter_mut() {
                let lower = key.to_lowercase();
                if lower.contains("pass")
                    || lower.contains("secret")
                    || lower.contains("token")
                    || lower.contains("key")
                {
                    if v.is_string() {
                        *v = serde_json::Value::String("***".into());
                    }
                } else {
                    redact_json_secrets(v);
                }
            }
        }
        serde_json::Value::Array(arr) => {
            for v in arr {
                redact_json_secrets(v);
            }
        }
        _ => {}
    }
}

fn merge_json_secrets(new_val: &mut serde_json::Value, cur_val: &serde_json::Value) {
    if let (serde_json::Value::Object(new_map), serde_json::Value::Object(cur_map)) = (new_val, cur_val) {
        for (k, v) in new_map.iter_mut() {
            if v.as_str() == Some("***") {
                if let Some(existing) = cur_map.get(k) {
                    *v = existing.clone();
                }
            } else if let Some(existing) = cur_map.get(k) {
                merge_json_secrets(v, existing);
            }
        }
    }
}

impl Settings {
    /// Возвращает безопасную копию настроек с замаскированными секретами.
    pub fn redacted(&self) -> Self {
        let mut copy = self.clone();
        redact_json_secrets(&mut copy.auth.params);
        copy
    }

    /// Объединяет новые настройки с текущими, сохраняя существующие секреты,
    /// если клиент прислал маскированное значение `"***"`.
    pub fn merge_preserving_secrets(&mut self, current: &Settings) {
        merge_json_secrets(&mut self.auth.params, &current.auth.params);
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
        let previous_mode = state.mode.lock().expect("mode store poisoned").global_mode;
        state.mode.lock().expect("mode store poisoned").global_mode = next.default_mode;
        crate::mode::ModeService::announce_global_mode(state, previous_mode, next.default_mode);
        next
    }

    /// Полная замена настроек с fallible persist: `/api/v1/settings` должен
    /// вернуть 503, если storage не сохранил документ, а память — откатиться.
    pub async fn try_set_async(state: &crate::state::AppState, next: Settings) -> Result<Settings, String> {
        let _persist = state.settings_persist.lock().await;
        let (previous, previous_mode, mode_changed, next, json_str) = {
            let mut settings = state.settings.lock().expect("settings store poisoned");
            let previous = settings.clone();
            let previous_mode = state.mode.lock().expect("mode store poisoned").global_mode;
            let mut next = next;
            next.merge_preserving_secrets(&settings);
            *settings = next.clone();
            let mode_changed = next.default_mode != previous.default_mode;
            if mode_changed {
                state.mode.lock().expect("mode store poisoned").global_mode = next.default_mode;
            }
            let json = match serde_json::to_string(&next) {
                Ok(json) => json,
                Err(error) => {
                    *settings = previous.clone();
                    if mode_changed {
                        state.mode.lock().expect("mode store poisoned").global_mode = previous_mode;
                    }
                    return Err(error.to_string());
                }
            };
            (previous, previous_mode, mode_changed, next, json)
        };
        match persist(state, &json_str).await {
            Ok(()) => {
                if mode_changed {
                    crate::mode::ModeService::announce_global_mode(state, previous_mode, next.default_mode);
                }
                Ok(next)
            }
            Err(error) => {
                *state.settings.lock().expect("settings store poisoned") = previous;
                if mode_changed {
                    state.mode.lock().expect("mode store poisoned").global_mode = previous_mode;
                    // Offers admitted during the persist window saw the transient
                    // mode; re-evict if the committed mode is MCU again.
                    if previous_mode == RoomMode::Mcu {
                        crate::mode::ModeService::announce_global_mode(state, next.default_mode, previous_mode);
                    }
                }
                Err(error)
            }
        }
    }

    /// Атомарно меняет одно поле настроек под lock'ом и сериализует persist.
    ///
    /// Нужен для codec-PATCH: вендорский UI шлёт audio и video двумя
    /// параллельными запросами, поэтому get → mutate → set терял один из
    /// списков. Замыкание выполняется под удерживаемым lock'ом настроек.
    pub async fn update_async<F>(state: &crate::state::AppState, update: F) -> Settings
    where
        F: FnOnce(&mut Settings),
    {
        match Self::try_update_async(state, update).await {
            Ok(settings) => settings,
            Err(error) => {
                tracing::warn!(%error, "failed to persist settings to storage");
                Self::get(state)
            }
        }
    }

    /// Как [`Self::update_async`], но ошибка persist возвращается вызывающему:
    /// codec-PATCH не должен отвечать 200, если настройка не сохранена.
    /// При ошибке память откатывается к предыдущему снимку.
    pub async fn try_update_async<F>(state: &crate::state::AppState, update: F) -> Result<Settings, String>
    where
        F: FnOnce(&mut Settings),
    {
        let _persist = state.settings_persist.lock().await;
        let (previous, previous_mode, mode_changed, next, json_str) = {
            let mut settings = state.settings.lock().expect("settings store poisoned");
            let previous = settings.clone();
            let previous_mode = state.mode.lock().expect("mode store poisoned").global_mode;
            update(&mut settings);
            // Не перетираем mode, если PATCH не менял default_mode: `/api/v1/mode`
            // управляет глобальным режимом отдельно от настроек.
            let mode_changed = settings.default_mode != previous.default_mode;
            if mode_changed {
                state.mode.lock().expect("mode store poisoned").global_mode = settings.default_mode;
            }
            let json = match serde_json::to_string(&*settings) {
                Ok(json) => json,
                Err(error) => {
                    *settings = previous.clone();
                    if mode_changed {
                        state.mode.lock().expect("mode store poisoned").global_mode = previous_mode;
                    }
                    return Err(error.to_string());
                }
            };
            (previous, previous_mode, mode_changed, settings.clone(), json)
        };
        match persist(state, &json_str).await {
            Ok(()) => {
                if mode_changed {
                    crate::mode::ModeService::announce_global_mode(state, previous_mode, next.default_mode);
                }
                Ok(next)
            }
            Err(error) => {
                *state.settings.lock().expect("settings store poisoned") = previous;
                if mode_changed {
                    state.mode.lock().expect("mode store poisoned").global_mode = previous_mode;
                    if previous_mode == RoomMode::Mcu {
                        crate::mode::ModeService::announce_global_mode(state, next.default_mode, previous_mode);
                    }
                }
                Err(error)
            }
        }
    }
}

async fn persist(state: &crate::state::AppState, json_str: &str) -> Result<(), String> {
    #[cfg(test)]
    if state.settings_persist_fail.load(std::sync::atomic::Ordering::SeqCst) {
        return Err("injected settings persist failure".to_string());
    }
    if let Some(storage) = &state.storage {
        storage.upsert_settings(json_str).await.map_err(|error| error.to_string())?;
    }
    Ok(())
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
    fn set_default_mode_mcu_evicts_real_peers() {
        use std::sync::{Arc, Mutex};
        let mut state = AppState::new();
        let room = vks_core::RoomId::new();
        state.rooms.lock().unwrap().insert(room, crate::state::RoomRecord::new(room, "conf", None));
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink = seen.clone();
        state.webrtc_evict = Some(Arc::new(move |target| sink.lock().unwrap().push(target)));
        SettingsService::set(&state, Settings { default_mode: RoomMode::Mcu, ..Default::default() });
        assert_eq!(*seen.lock().unwrap(), vec![Some(room)]);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn concurrent_codec_updates_are_atomic() {
        use std::sync::Arc;
        let state = Arc::new(AppState::new());
        let a = state.clone();
        let b = state.clone();
        let (audio, video) = tokio::join!(
            async move { SettingsService::update_async(&a, |settings| settings.codecs.audio = vec!["OPUS".into()]).await },
            async move { SettingsService::update_async(&b, |settings| settings.codecs.video = vec!["H265".into()]).await },
        );
        assert_eq!(audio.codecs.audio, vec!["OPUS".to_string()]);
        assert_eq!(video.codecs.video, vec!["H265".to_string()]);
        let current = SettingsService::get(&state);
        assert_eq!(current.codecs.audio, vec!["OPUS".to_string()]);
        assert_eq!(current.codecs.video, vec!["H265".to_string()]);
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
    #[test]
    fn default_codec_matrix_entries_map_to_specs() {
        // Every name the admin ships as operator intent must resolve in the
        // codec SSOT; otherwise "unsupported" rows would be invisible to the
        // matrix and the API could not explain them.
        use vks_common::codec_spec;
        let config = CodecConfig::default();
        for name in &config.video {
            assert!(codec_spec::lookup(name).is_some(), "video codec {name} has no CodecSpec");
        }
        for entry in &config.audio {
            let (name, bitrate) = CodecConfig::audio_parts(entry);
            assert!(
                codec_spec::lookup_variant(&name, bitrate).is_some() || codec_spec::family(&name).is_some(),
                "audio codec {entry} has no CodecSpec"
            );
        }
    }
}
