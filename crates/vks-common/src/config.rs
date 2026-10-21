//! Загрузка конфигурации (figment) + env-override `VKS_*`.
//!
//! Основание: `docs/rules/BACKEND_RULES.md` §6.3 (Secrets: env vars VKS_*),
//! `docs/DEVELOPMENT_PLAN.md` Фаза 0 (config.rs — figment, env override).

use figment::providers::{Env, Format, Serialized, Toml};
use serde::{Deserialize, Serialize};
use uncased::Uncased;

/// Базовая конфигурация сервера (покрывает требования R6: имя, IP, БД, LDAP).
/// Полная структура настраиваемости расширяется в фазах 5/7.
#[derive(Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Config {
    /// Имя сервера (требование R6).
    pub server_name: String,
    /// Адрес прослушивания signaling/admin (R6: IP).
    pub listen_addr: String,
    /// Стратегия аутентификации: local | ldap | oidc (ADR-0022, BACKEND_RULES §2.1).
    pub auth_provider: String,
    /// DSN БД (R6: БД).
    pub database_url: String,
    /// URL LDAP (R6: LDAP).
    pub ldap_url: String,
    /// Глобальный режим по умолчанию (ADR-0026, Q7): "sfu" | "mcu".
    pub default_room_mode: String,
    /// Каталог рантайма статики фронтенда PNM (`index.html` + assets;
    /// ADR-0027, docs/progress/archive/FRONTEND_PORT_PLAN.md §4.2). Дефолт — относительно CWD процесса;
    /// переопределение: `VKS_FRONTEND_DIR`.
    pub frontend_dir: String,
    /// Секрет HS256 для `/api/v3` (ADR-0027 §4.3; `VKS_JWT_SECRET`).
    /// Пустой → v3 fail-closed (503), сервер работает; долг: RS256 (§4.3).
    pub jwt_secret: String,
    /// TTL access-токенов `/api/v3`, сек (`VKS_JWT_TTL_SECS`).
    /// Дефолт 86400: legacy UI не умеет refresh (§4.3, отклонение в ADR-0027).
    pub jwt_ttl_secs: i64,
    /// Логин оператора, bootstrapping в LocalDb (`VKS_ADMIN_LOGIN`; dev: `admin`).
    pub admin_login: String,
    /// Пароль оператора (`VKS_ADMIN_PASSWORD`). Нужен для первого bootstrap
    /// или разовой ротации слабой БД; на каждом старте не обязателен.
    pub admin_password: String,
    /// Адрес прослушивания WebRTC UDP-трафика (`VKS_MEDIA_LISTEN_ADDR`; dev: `127.0.0.1:0`).
    pub media_listen_addr: String,
    /// Разрешить ли fallback на MemoryStorage при отказе БД (`VKS_STORAGE_ALLOW_FALLBACK`; dev: false).
    /// По умолчанию: false (fail-fast: сбой подключения к сконфигурированной БД прерывает startup).
    pub storage_allow_fallback: bool,
    /// Разрешить небезопасные dev-дефолты (`admin/admin`, короткий JWT).
    /// Secure-by-default: `false`; `configs/default.toml` явно включает `true`.
    /// В production сервер отказывается стартовать с дефолтным оператором,
    /// известным dev-секретом или коротким JWT.
    pub allow_insecure_defaults: bool,
    /// Список адресов ICE-серверов (STUN/TURN, e.g. "stun:stun.l.google.com:19302") (`VKS_ICE_SERVERS`).
    pub ice_servers: Vec<String>,
    /// Публичный или LAN IP кандидата WebRTC для анонса в SDP (`VKS_CANDIDATE_ADDR`).
    pub candidate_addr: Option<String>,
}

#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum ConfigError {
    #[error("invalid auth_provider '{0}': expected 'local', 'ldap', or 'oidc'")]
    InvalidAuthProvider(String),
    #[error("invalid default_room_mode '{0}': expected 'sfu' or 'mcu'")]
    InvalidRoomMode(String),
    #[error("invalid jwt_ttl_secs '{0}': must be positive")]
    InvalidJwtTtl(i64),
    #[error("invalid listen_addr '{0}': {1}")]
    InvalidListenAddr(String, String),
    #[error("invalid media_listen_addr '{0}': {1}")]
    InvalidMediaListenAddr(String, String),
    #[error("invalid candidate_addr '{0}': must be a valid IP address")]
    InvalidCandidateAddr(String),
    #[error("insecure defaults are disabled but jwt_secret is shorter than 32 bytes or is the published dev secret")]
    WeakJwtSecret,
    #[error("insecure defaults are disabled but admin_password is a known/weak/placeholder value (set a strong VKS_ADMIN_PASSWORD)")]
    InsecureAdminPassword,
    #[error(
        "insecure defaults are disabled but database_url is empty, in-memory, a CHANGE_ME placeholder, or storage_allow_fallback=true"
    )]
    NoPersistentStorage,
}

/// Опубликованный dev-секрет из `configs/default.toml`: в prod запрещён.
pub const DEV_JWT_SECRET: &str = "dev_ec994ad4e03aa63eafc3131719a0a50cce4dbfb474c855b0";

/// Маскирует строковые секреты.
pub fn redact_secret(s: &str) -> &'static str {
    if s.is_empty() {
        "<empty>"
    } else {
        "***"
    }
}

/// Маскирует учетные данные в DSN/URI:
/// `postgres://user:pass@host/db` -> `postgres://user:***@host/db`
/// `postgres://:pass@host/db` -> `postgres://:***@host/db`
/// `postgres://user:@host/db` -> `postgres://user:***@host/db`
/// `user:pass@host` -> `user:***@host`
/// `sqlite://db?password=secret` -> `sqlite://db?password=***`
pub fn redact_dsn(dsn: &str) -> String {
    if dsn.is_empty() {
        return String::new();
    }
    let mut result = dsn.to_string();

    // 1. Redact credentials before @
    if let Some((before_at, host_and_path)) = result.split_once('@') {
        if let Some((scheme, creds)) = before_at.split_once("://") {
            if let Some((user, _pass)) = creds.split_once(':') {
                result = format!("{scheme}://{user}:***@{host_and_path}");
            } else {
                result = format!("{scheme}://***@{host_and_path}");
            }
        } else if let Some((user, _pass)) = before_at.split_once(':') {
            result = format!("{user}:***@{host_and_path}");
        } else {
            result = format!("***@{host_and_path}");
        }
    }

    // 2. Redact query parameters containing secrets/passwords/keys/tokens
    if let Some((base, query)) = result.split_once('?') {
        let mut redacted_params = Vec::new();
        for param in query.split('&') {
            if let Some((k, _v)) = param.split_once('=') {
                let lower_k = k.to_lowercase();
                if lower_k.contains("pass")
                    || lower_k.contains("secret")
                    || lower_k.contains("token")
                    || lower_k.contains("key")
                {
                    redacted_params.push(format!("{k}=***"));
                } else {
                    redacted_params.push(param.to_string());
                }
            } else {
                redacted_params.push(param.to_string());
            }
        }
        result = format!("{base}?{}", redacted_params.join("&"));
    }

    // 3. Redact key-value formatted DSNs (e.g. "host=localhost password=secret dbname=vks")
    if !result.contains('@') && !result.contains('?') && result.contains('=') {
        let parts: Vec<&str> = result.split_whitespace().collect();
        if parts.iter().any(|p| p.contains('=')) {
            let mut redacted_parts = Vec::new();
            for part in parts {
                if let Some((k, _v)) = part.split_once('=') {
                    let lower_k = k.to_lowercase();
                    if lower_k.contains("pass")
                        || lower_k.contains("secret")
                        || lower_k.contains("token")
                        || lower_k.contains("key")
                    {
                        redacted_parts.push(format!("{k}=***"));
                    } else {
                        redacted_parts.push(part.to_string());
                    }
                } else {
                    redacted_parts.push(part.to_string());
                }
            }
            result = redacted_parts.join(" ");
        }
    }

    result
}

impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Config")
            .field("server_name", &self.server_name)
            .field("listen_addr", &self.listen_addr)
            .field("auth_provider", &self.auth_provider)
            .field("database_url", &redact_dsn(&self.database_url))
            .field("ldap_url", &redact_dsn(&self.ldap_url))
            .field("default_room_mode", &self.default_room_mode)
            .field("frontend_dir", &self.frontend_dir)
            .field("jwt_secret", &redact_secret(&self.jwt_secret))
            .field("jwt_ttl_secs", &self.jwt_ttl_secs)
            .field("admin_login", &self.admin_login)
            .field("admin_password", &redact_secret(&self.admin_password))
            .field("media_listen_addr", &self.media_listen_addr)
            .field("storage_allow_fallback", &self.storage_allow_fallback)
            .field("ice_servers", &self.ice_servers)
            .field("candidate_addr", &self.candidate_addr)
            .finish()
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            server_name: "vks".into(),
            listen_addr: "0.0.0.0:443".into(),
            auth_provider: "local".into(),
            database_url: String::new(),
            ldap_url: String::new(),
            default_room_mode: "sfu".into(),
            frontend_dir: "client/pnm-frontend/deployed/pnm".into(),
            jwt_secret: String::new(),
            jwt_ttl_secs: 86_400,
            admin_login: "admin".into(),
            admin_password: String::new(),
            media_listen_addr: "127.0.0.1:0".into(),
            storage_allow_fallback: false,
            allow_insecure_defaults: false,
            ice_servers: Vec::new(),
            candidate_addr: None,
        }
    }
}

impl Config {
    /// Загружает конфигурацию из (в порядке приоритета, от меньшего к большему):
    /// 1. значения по умолчанию;
    /// 2. TOML-файл (если путь задан);
    /// 3. переменные окружения `VKS_*` (например, `VKS_SERVER_NAME`).
    ///
    /// Env-переменные имеют **наивысший** приоритет (BACKEND_RULES §6.3).
    #[allow(clippy::result_large_err)] // figment::Error is large (contains profile metadata)
    pub fn load(toml_path: Option<&str>) -> Result<Self, figment::Error> {
        // 1. дефолты (Config → Serialized → Provider).
        let mut fig = figment::Figment::from(Serialized::defaults(Config::default()));

        // 2. TOML (опционально).
        if let Some(path) = toml_path {
            fig = fig.merge(Toml::file(path));
        }

        // 3. env-override VKS_* (наивысший приоритет).
        // Ключи приводятся к нижнему регистру (VKS_SERVER_NAME → server_name).
        fig = fig.merge(Env::prefixed("VKS_").map(|key| Uncased::new(key.as_str().to_lowercase())));

        fig.extract()
    }

    /// Известные weak/placeholder-пароли (точные варианты для проверки
    /// `LocalDb::authenticate`, который case-sensitive).
    pub const KNOWN_WEAK_PASSWORDS: &'static [&'static str] = &[
        "admin",
        "admin_password",
        "changeme",
        "change_me",
        "change_me_strong_password",
        "password",
        "secret",
        "CHANGE_ME",
        "CHANGE_ME_STRONG_PASSWORD",
    ];

    /// Возвращает true для дефолтных/placeholder-паролей, которые нельзя
    /// использовать в production. Общая функция для config- и DB-проверок.
    /// Ограничение: произвольный короткий пароль по хэшу не отличить от
    /// пользовательского; такие пароли ловятся только при заданном
    /// `admin_password`/через UI-ротацию.
    pub fn is_weak_password(password: &str) -> bool {
        let p = password.trim();
        p.is_empty()
            || p.len() < 12
            || Self::KNOWN_WEAK_PASSWORDS.iter().any(|w| p.eq_ignore_ascii_case(w))
            || p.to_ascii_uppercase().contains("CHANGE_ME")
    }

    /// Валидирует значения полей конфигурации (M0.3).
    pub fn validate(&self) -> Result<(), ConfigError> {
        let provider = self.auth_provider.trim().to_lowercase();
        if provider != "local" && provider != "ldap" && provider != "oidc" {
            return Err(ConfigError::InvalidAuthProvider(self.auth_provider.clone()));
        }

        let mode = self.default_room_mode.trim().to_lowercase();
        if mode != "sfu" && mode != "mcu" {
            return Err(ConfigError::InvalidRoomMode(self.default_room_mode.clone()));
        }

        if self.jwt_ttl_secs <= 0 {
            return Err(ConfigError::InvalidJwtTtl(self.jwt_ttl_secs));
        }

        if let Err(e) = self.listen_addr.parse::<std::net::SocketAddr>() {
            return Err(ConfigError::InvalidListenAddr(self.listen_addr.clone(), e.to_string()));
        }

        if let Err(e) = self.media_listen_addr.parse::<std::net::SocketAddr>() {
            return Err(ConfigError::InvalidMediaListenAddr(self.media_listen_addr.clone(), e.to_string()));
        }

        if let Some(ref cand) = self.candidate_addr {
            let trimmed = cand.trim();
            if !trimmed.is_empty() && trimmed.parse::<std::net::IpAddr>().is_err() {
                return Err(ConfigError::InvalidCandidateAddr(cand.clone()));
            }
        }

        // Prod fail-closed: dev-дефолты не должны переезжать в production.
        // admin_password опционален на каждом старте: он нужен только для
        // первого bootstrap или для разовой ротации слабой БД. Если он задан,
        // то обязан быть сильным; пустой проверяется после гидратации БД.
        if !self.allow_insecure_defaults {
            let password = self.admin_password.trim();
            if !password.is_empty() && Self::is_weak_password(password) {
                return Err(ConfigError::InsecureAdminPassword);
            }
            let jwt = self.jwt_secret.trim();
            if jwt.len() < 32 || jwt == DEV_JWT_SECRET {
                return Err(ConfigError::WeakJwtSecret);
            }
            if self.storage_allow_fallback
                || self.database_url.trim().is_empty()
                || self.database_url.contains(":memory:")
                || self.database_url.to_ascii_uppercase().contains("CHANGE_ME")
            {
                return Err(ConfigError::NoPersistentStorage);
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    fn dev_config() -> Config {
        Config { allow_insecure_defaults: true, ..Config::default() }
    }

    // Защита от гонок при параллельных тестах: env-переменные глобальны,
    // поэтому тест, который их устанавливает, держит мьютекс.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn defaults_apply() {
        let c = Config::default();
        assert_eq!(c.server_name, "vks");
        assert_eq!(c.default_room_mode, "sfu");
        assert_eq!(c.frontend_dir, "client/pnm-frontend/deployed/pnm");
        // v3-auth (§4.3): пустой секрет = fail-closed; TTL 86400 (legacy без refresh).
        assert_eq!(c.jwt_secret, "");
        assert_eq!(c.jwt_ttl_secs, 86_400);
        assert_eq!(c.admin_login, "admin");
        assert_eq!(c.admin_password, "");
        assert_eq!(c.media_listen_addr, "127.0.0.1:0");
        assert_eq!(c.candidate_addr, None);
    }

    #[test]
    fn load_without_toml_gives_defaults() {
        // Держим тот же мьютекс, чтобы env-тест не менял VKS_SERVER_NAME
        // в момент этого чтения (эвристика: env-тест устанавливает значение).
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let c = Config::load(None).expect("load should succeed with defaults");
        assert_eq!(c.server_name, "vks");
        assert_eq!(c.listen_addr, "0.0.0.0:443");
    }

    #[test]
    fn env_override_wins() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let var = "VKS_SERVER_NAME";
        let saved = std::env::var(var).ok();
        std::env::set_var(var, "from-env-unique");
        let c = Config::load(None).expect("load with env");
        assert_eq!(c.server_name, "from-env-unique");
        match saved {
            Some(v) => std::env::set_var(var, v),
            None => std::env::remove_var(var),
        }
    }

    #[test]
    fn env_override_jwt_fields() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let saved_secret = std::env::var("VKS_JWT_SECRET").ok();
        let saved_ttl = std::env::var("VKS_JWT_TTL_SECS").ok();
        std::env::set_var("VKS_JWT_SECRET", "env-secret-not-logged");
        std::env::set_var("VKS_JWT_TTL_SECS", "3600");
        let c = Config::load(None).expect("load with jwt env");
        assert_eq!(c.jwt_secret, "env-secret-not-logged");
        assert_eq!(c.jwt_ttl_secs, 3600);
        match saved_secret {
            Some(v) => std::env::set_var("VKS_JWT_SECRET", v),
            None => std::env::remove_var("VKS_JWT_SECRET"),
        }
        match saved_ttl {
            Some(v) => std::env::set_var("VKS_JWT_TTL_SECS", v),
            None => std::env::remove_var("VKS_JWT_TTL_SECS"),
        }
    }

    /// TOML-файл — отдельный путь загрузки (не только env + defaults).
    /// Записываем временный TOML в `tempdir`-каталог процесса, читаем через
    /// `Config::load(Some(path))` и проверяем override полей.
    #[test]
    fn toml_file_load_overrides_defaults() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        // Временный файл внутри target-каталога крейта (гарантированно writable).
        let dir = std::env::temp_dir().join(format!("vks-common-config-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let path = dir.join("test.toml");
        std::fs::write(
            &path,
            r#"
server_name = "from-toml-file"
listen_addr = "127.0.0.1:8443"
auth_provider = "ldap"
database_url = "postgres://user:pass@localhost/vks"
ldap_url = "ldap://ldap.example.com:389"
default_room_mode = "mcu"
"#,
        )
        .expect("write temp toml");
        let c = Config::load(Some(path.to_str().expect("valid utf8 path"))).expect("load from toml");
        assert_eq!(c.server_name, "from-toml-file");
        assert_eq!(c.listen_addr, "127.0.0.1:8443");
        assert_eq!(c.auth_provider, "ldap");
        assert_eq!(c.database_url, "postgres://user:pass@localhost/vks");
        assert_eq!(c.ldap_url, "ldap://ldap.example.com:389");
        assert_eq!(c.default_room_mode, "mcu");
        let _ = std::fs::remove_file(&path);
    }

    /// Env имеет приоритет над TOML-файлом (BACKEND_RULES §6.3: env — наивысший).
    #[test]
    fn env_beats_toml_file() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let dir = std::env::temp_dir().join(format!("vks-common-config-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let path = dir.join("priority.toml");
        std::fs::write(&path, "server_name = \"from-toml\"\n").expect("write temp toml");
        let var = "VKS_SERVER_NAME";
        let saved = std::env::var(var).ok();
        std::env::set_var(var, "from-env-wins");
        let c = Config::load(Some(path.to_str().expect("valid utf8 path"))).expect("load toml + env");
        assert_eq!(c.server_name, "from-env-wins", "env must override toml file value");
        match saved {
            Some(v) => std::env::set_var(var, v),
            None => std::env::remove_var(var),
        }
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn redaction_masks_secrets_and_dsn() {
        assert_eq!(redact_secret("secret_123"), "***");
        assert_eq!(redact_secret(""), "<empty>");
        assert_eq!(
            redact_dsn("postgres://admin:super_secret_pw@10.0.0.1:5432/vks_db"),
            "postgres://admin:***@10.0.0.1:5432/vks_db"
        );
        assert_eq!(redact_dsn("sqlite://app.db"), "sqlite://app.db");

        let cfg = Config {
            database_url: "postgres://vks_user:mypassword@localhost:5432/vks".into(),
            jwt_secret: "super_secret_jwt".into(),
            admin_password: "admin_cleartext_password".into(),
            ..dev_config()
        };
        let debug_str = format!("{cfg:?}");
        assert!(!debug_str.contains("super_secret_jwt"), "jwt_secret leaked in Debug");
        assert!(!debug_str.contains("admin_cleartext_password"), "admin_password leaked in Debug");
        assert!(!debug_str.contains("mypassword"), "database_url password leaked in Debug");
        assert!(debug_str.contains("postgres://vks_user:***@localhost:5432/vks"));
    }

    #[test]
    fn validate_accepts_valid_config() {
        let cfg = dev_config();
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn validate_rejects_invalid_auth_provider() {
        let cfg = Config { auth_provider: "invalid_provider".into(), ..dev_config() };
        assert_eq!(cfg.validate(), Err(ConfigError::InvalidAuthProvider("invalid_provider".into())));
    }

    #[test]
    fn validate_rejects_invalid_room_mode() {
        let cfg = Config { default_room_mode: "invalid_mode".into(), ..dev_config() };
        assert_eq!(cfg.validate(), Err(ConfigError::InvalidRoomMode("invalid_mode".into())));
    }

    #[test]
    fn validate_rejects_non_positive_jwt_ttl() {
        let cfg0 = Config { jwt_ttl_secs: 0, ..dev_config() };
        assert_eq!(cfg0.validate(), Err(ConfigError::InvalidJwtTtl(0)));
        let cfg_neg = Config { jwt_ttl_secs: -100, ..dev_config() };
        assert_eq!(cfg_neg.validate(), Err(ConfigError::InvalidJwtTtl(-100)));
    }

    #[test]
    fn validate_rejects_invalid_socket_addrs() {
        let cfg = Config { listen_addr: "not_an_ip".into(), ..dev_config() };
        assert!(matches!(cfg.validate(), Err(ConfigError::InvalidListenAddr(_, _))));

        let cfg2 = Config { media_listen_addr: "999.999.999.999:7777".into(), ..dev_config() };
        assert!(matches!(cfg2.validate(), Err(ConfigError::InvalidMediaListenAddr(_, _))));
    }

    #[test]
    fn validate_candidate_addr_accepts_valid_ip() {
        let mut cfg = Config { candidate_addr: Some("192.168.1.93".into()), ..dev_config() };
        assert!(cfg.validate().is_ok());

        cfg.candidate_addr = Some("::1".into());
        assert!(cfg.validate().is_ok());

        cfg.candidate_addr = Some("".into());
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn validate_candidate_addr_rejects_invalid_ip() {
        let mut cfg = Config { candidate_addr: Some("not-an-ip".into()), ..dev_config() };
        assert_eq!(cfg.validate(), Err(ConfigError::InvalidCandidateAddr("not-an-ip".into())));

        cfg.candidate_addr = Some("192.168.1.999".into());
        assert_eq!(cfg.validate(), Err(ConfigError::InvalidCandidateAddr("192.168.1.999".into())));
    }

    #[test]
    fn redact_dsn_handles_query_passwords_and_edge_cases() {
        assert_eq!(redact_dsn(""), "");
        assert_eq!(
            redact_dsn("sqlite://db.sqlite?cache=shared&password=secret123&mode=rwc"),
            "sqlite://db.sqlite?cache=shared&password=***&mode=rwc"
        );
        assert_eq!(
            redact_dsn("postgres://user:p@host:5432/db?sslmode=require&secret_key=abc"),
            "postgres://user:***@host:5432/db?sslmode=require&secret_key=***"
        );
        assert_eq!(redact_dsn("user:password@host"), "user:***@host");
        assert_eq!(redact_dsn("secret@host"), "***@host");
        assert_eq!(
            redact_dsn("host=localhost port=5432 user=postgres password=secret123 dbname=vks"),
            "host=localhost port=5432 user=postgres password=*** dbname=vks"
        );
    }

    #[test]
    fn validate_rejects_insecure_defaults_in_prod_mode() {
        let cfg = Config {
            allow_insecure_defaults: false,
            admin_password: "admin".into(),
            jwt_secret: "x".repeat(48),
            ..dev_config()
        };
        assert_eq!(cfg.validate(), Err(ConfigError::InsecureAdminPassword));
    }

    #[test]
    fn validate_rejects_short_jwt_in_prod_mode() {
        let cfg = Config {
            allow_insecure_defaults: false,
            admin_password: "strong-production-password".into(),
            jwt_secret: "short".into(),
            ..dev_config()
        };
        assert_eq!(cfg.validate(), Err(ConfigError::WeakJwtSecret));
    }

    #[test]
    fn validate_accepts_strong_prod_config() {
        let cfg = Config {
            allow_insecure_defaults: false,
            admin_password: "strong-production-password".into(),
            jwt_secret: "x".repeat(48),
            database_url: "postgres://vks:pass@db:5432/vks".into(),
            ..dev_config()
        };
        assert!(cfg.validate().is_ok(), "{:?}", cfg.validate());
    }

    #[test]
    fn prod_example_without_secrets_is_rejected() {
        let _guard = ENV_LOCK.lock().unwrap();
        let vars = [
            "VKS_ADMIN_PASSWORD",
            "VKS_JWT_SECRET",
            "VKS_ALLOW_INSECURE_DEFAULTS",
            "VKS_DATABASE_URL",
            "VKS_STORAGE_ALLOW_FALLBACK",
        ];
        let saved: Vec<_> = vars.iter().map(|v| (*v, std::env::var(v).ok())).collect();
        for v in vars {
            std::env::remove_var(v);
        }
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../configs/prod.example.toml");
        let cfg = Config::load(Some(path)).expect("prod example must parse");
        let result = cfg.validate();
        for (v, value) in saved {
            match value {
                Some(value) => std::env::set_var(v, value),
                None => std::env::remove_var(v),
            }
        }
        assert_eq!(result, Err(ConfigError::WeakJwtSecret));
    }

    #[test]
    fn prod_mode_allows_empty_admin_password_if_jwt_and_storage_are_strong() {
        let cfg = Config {
            allow_insecure_defaults: false,
            admin_password: String::new(),
            jwt_secret: "x".repeat(48),
            database_url: "postgres://vks:pass@db:5432/vks".into(),
            ..dev_config()
        };
        assert!(cfg.validate().is_ok(), "{:?}", cfg.validate());
    }

    #[test]
    fn prod_mode_rejects_placeholder_password_and_memory_storage() {
        let weak = Config {
            allow_insecure_defaults: false,
            admin_password: "CHANGE_ME_STRONG_PASSWORD".into(),
            jwt_secret: "x".repeat(48),
            database_url: "postgres://vks:pass@db:5432/vks".into(),
            ..dev_config()
        };
        assert_eq!(weak.validate(), Err(ConfigError::InsecureAdminPassword));

        let memory = Config {
            allow_insecure_defaults: false,
            admin_password: String::new(),
            jwt_secret: "x".repeat(48),
            database_url: String::new(),
            ..dev_config()
        };
        assert_eq!(memory.validate(), Err(ConfigError::NoPersistentStorage));

        let placeholder = Config {
            allow_insecure_defaults: false,
            admin_password: String::new(),
            jwt_secret: "x".repeat(48),
            database_url: "postgres://vks:CHANGE_ME@db:5432/vks".into(),
            ..dev_config()
        };
        assert_eq!(placeholder.validate(), Err(ConfigError::NoPersistentStorage));
    }

    #[test]
    fn dev_config_explicitly_opts_into_insecure_defaults() {
        let _guard = ENV_LOCK.lock().unwrap();
        let saved: Vec<_> = ["VKS_JWT_SECRET", "VKS_ADMIN_PASSWORD", "VKS_ALLOW_INSECURE_DEFAULTS"]
            .iter()
            .map(|v| (*v, std::env::var(v).ok()))
            .collect();
        for v in ["VKS_JWT_SECRET", "VKS_ADMIN_PASSWORD", "VKS_ALLOW_INSECURE_DEFAULTS"] {
            std::env::remove_var(v);
        }
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../configs/default.toml");
        let cfg = Config::load(Some(path)).expect("dev config must parse");
        assert!(cfg.allow_insecure_defaults);
        assert_eq!(cfg.jwt_secret, DEV_JWT_SECRET);
        for (v, value) in saved {
            match value {
                Some(value) => std::env::set_var(v, value),
                None => std::env::remove_var(v),
            }
        }
    }

    #[test]
    fn prod_mode_rejects_published_dev_jwt_secret() {
        let cfg = Config {
            allow_insecure_defaults: false,
            admin_password: "strong-production-password".into(),
            jwt_secret: DEV_JWT_SECRET.into(),
            ..dev_config()
        };
        assert_eq!(cfg.validate(), Err(ConfigError::WeakJwtSecret));
    }

    #[test]
    fn dev_mode_allows_dev_defaults_explicitly() {
        let cfg = Config {
            allow_insecure_defaults: true,
            admin_password: "admin".into(),
            jwt_secret: "short".into(),
            ..Config::default()
        };
        assert!(cfg.validate().is_ok());
    }
}
