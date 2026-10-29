//! `vks-server` — главный бинарник (композиционный корень).
//!
//! Фаза 7: composition root сервера ВКС.
//! - Загрузка конфигурации (`Config::load`) со fallback-цепочкой: defaults -> TOML -> VKS_* env.
//! - Инициализация логирования и трейсинга (`vks_common::tracing`).
//! - Регистрация Prometheus-метрик (`vks_common::metrics`).
//! - Подключение постоянного хранилища (SQLite / Memory).
//! - Инициализация состояния админ-сервера (rooms/users/settings/audit).
//! - Bootstrap `/api/v3` auth (ADR-0027 §4.3): `JwtIssuer` (`VKS_JWT_SECRET`) +
//!   оператор в `LocalDb` (`VKS_ADMIN_LOGIN`/`VKS_ADMIN_PASSWORD`); пустой секрет →
//!   v3 fail-closed (503), сервер продолжает работать.
//! - Загрузка статики фронтенда PNM (`VKS_FRONTEND_DIR`, ADR-0027).
//! - Сборка приложения `vks_server::build_app`:
//!   admin REST (`/api/v1/...`), legacy-адаптер UI (`/api/v3/...`), `/healthz`, `/readyz`,
//!   метрики (`/metrics`), сигналинг (`/ws`), статика UI (`/pnm/`, `/mcu/*`).
//! - Graceful shutdown на сигналы SIGINT (Ctrl+C) и SIGTERM.

use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;

use tracing::{error, info, warn};

use vks_admin::state::AppState;
use vks_auth::jwt::{JwtConfig, JwtIssuer};
use vks_auth::local_db::LocalDb;
use vks_common::config::Config;
use vks_common::metrics::{init_default as init_metrics, FRONTEND_DENIED};
use vks_common::tracing::{init as init_tracing, LogFormat};
use vks_core::ports::Storage;
use vks_server::frontend::Frontend;
use vks_server::{build_app_with_services, ClusterManager, GatewayManager, RecorderManager};
use vks_signaling::handler::HandlerConfig;
use vks_signaling::sdp::RtpMap;
use vks_storage::{MemoryStorage, PostgresStorage, SqliteStorage};

/// Ошибка конфигурации: отдельный exit-код 78 (EX_CONFIG), чтобы systemd с
/// `RestartPreventExitStatus=78` не повторял заведомо неудачный запуск.
fn config_exit(err: impl std::fmt::Display) -> ! {
    error!(error = %err, "configuration rejected; aborting startup");
    std::process::exit(78);
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 1. Инициализация трейсинга
    init_tracing(None, LogFormat::Human);
    info!("vks-server starting up...");

    // 2. Инициализация метрик Prometheus
    init_metrics();

    // 3. Загрузка конфигурации
    let mut args = std::env::args().skip(1);
    let mut config_path = None;
    while let Some(arg) = args.next() {
        if arg == "-c" || arg == "--config" {
            config_path = args.next();
            break;
        } else if !arg.starts_with('-') && config_path.is_none() {
            config_path = Some(arg);
        }
    }
    let config_path = config_path.or_else(|| std::env::var("VKS_CONFIG_PATH").ok()).or_else(|| {
        // В release нельзя молча подхватить dev-конфиг из CWD: production
        // обязан передать --config/VKS_CONFIG_PATH явно.
        if cfg!(debug_assertions) && std::path::Path::new("configs/default.toml").exists() {
            Some("configs/default.toml".to_string())
        } else {
            None
        }
    });

    let config = match Config::load(config_path.as_deref()) {
        Ok(c) => {
            info!(path = ?config_path, "configuration loaded successfully");
            c
        }
        Err(e) => {
            if config_path.is_some() {
                config_exit(format!("failed to load configured config file {config_path:?}: {e}"));
            } else {
                config_exit(format!("failed to load configuration: {e}"));
            }
        }
    };

    // M0.3: Валидация конфигурации перед продолжением старта.
    // 78 (EX_CONFIG) — отдельный код, чтобы systemd не устраивал restart-loop.
    if let Err(e) = config.validate() {
        config_exit(e);
    }

    // Fail before touching storage: an empty or unusable candidate must exit 78
    // without creating/locking the SQLite database (the later preflight runs
    // after storage init and also validates the *derived* address and ports).
    // Dev configs (`allow_insecure_defaults`) keep the loopback escape hatch.
    if vks_server::webrtc_runtime::enabled() && !config.allow_insecure_defaults {
        let configured = config.candidate_addr.as_deref().unwrap_or("");
        if configured.trim().is_empty() {
            config_exit(
                "VKS_WEBRTC_TRANSPORT is enabled but candidate_addr is empty; set VKS_CANDIDATE_ADDR/network.candidate_addr (or VKS_WEBRTC_TRANSPORT=0)".to_string(),
            );
        }
        if let Err(error) = vks_server::webrtc_runtime::preflight_candidate(configured, false) {
            config_exit(format!("WebRTC preflight failed: {error}"));
        }
    }

    // Один владелец SQLite-файла: CLI dedup берёт тот же lock, поэтому не может
    // сделать backup/--apply, пока сервер держит состояние в памяти.
    let _db_lock = match vks_storage::db_lock::sqlite_path_from_url(&config.database_url) {
        Some(path) => match vks_storage::db_lock::DbLock::acquire(&path) {
            Ok(lock) => Some(lock),
            Err(e) => config_exit(format!("failed to acquire storage lock: {e}")),
        },
        None => None,
    };

    info!(
        version = env!("CARGO_PKG_VERSION"),
        server_name = %config.server_name,
        listen_addr = %config.listen_addr,
        room_mode = %config.default_room_mode,
        frontend_dir = %config.frontend_dir,
        "configuration applied"
    );

    // 4. Инициализация хранилища (SQLite / Postgres / Memory)
    // M1.6: отказ сконфигурированной БД прерывает startup (fail-fast),
    // если явно не разрешён fallback (storage_allow_fallback = true).
    let storage: Arc<dyn Storage> = if config.database_url.starts_with("sqlite:") {
        info!(url = %vks_common::config::redact_dsn(&config.database_url), "connecting to SQLite database");
        match SqliteStorage::init(&config.database_url).await {
            Ok(sqlite) => Arc::new(sqlite),
            Err(e) => {
                if config.storage_allow_fallback {
                    warn!(error = %e, "failed to initialize SQLite, falling back to MemoryStorage per storage_allow_fallback");
                    Arc::new(MemoryStorage::new())
                } else {
                    error!(error = %e, "failed to initialize configured SQLite database; aborting startup (fail-fast)");
                    return Err(e.into());
                }
            }
        }
    } else if config.database_url.starts_with("postgres://") || config.database_url.starts_with("postgresql://") {
        info!(url = %vks_common::config::redact_dsn(&config.database_url), "connecting to PostgreSQL database");
        match PostgresStorage::init(&config.database_url).await {
            Ok(pg) => Arc::new(pg),
            Err(e) => {
                if config.storage_allow_fallback {
                    warn!(error = %e, "failed to initialize PostgreSQL, falling back to MemoryStorage per storage_allow_fallback");
                    Arc::new(MemoryStorage::new())
                } else {
                    error!(error = %e, "failed to initialize configured PostgreSQL database; aborting startup (fail-fast)");
                    return Err(e.into());
                }
            }
        }
    } else if !config.database_url.is_empty() {
        error!(url = %vks_common::config::redact_dsn(&config.database_url), "unsupported database scheme in database_url; aborting startup");
        return Err(format!(
            "unsupported database scheme in database_url: {}",
            vks_common::config::redact_dsn(&config.database_url)
        )
        .into());
    } else {
        info!("using in-memory storage (database_url not configured)");
        Arc::new(MemoryStorage::new())
    };

    // 5. Инициализация состояния админ-сервера + bootstrap auth `/api/v3` (§4.3).
    let local_db = Arc::new(LocalDb::new());
    let mut admin_state = AppState::with_storage(storage.clone());
    admin_state.local_db = Some(local_db.clone());

    // T10: one-shot idempotent migration of `resources.directory` /
    // `resources.addresses` from the legacy configuration document into SQL.
    // Runs AFTER storage init (which applies migration 0007) and BEFORE
    // `sync_from_storage`. Any failure aborts startup even when
    // `storage_allow_fallback` is set: serving an empty directory and
    // accepting writes into RAM would silently lose data (T10 P1-5).
    if let Err(error) = vks_admin::api_v3::directory_migrate::run(&admin_state).await {
        config_exit(error);
    }

    if let Err(e) = admin_state.sync_from_storage().await {
        if !config.storage_allow_fallback && !config.database_url.is_empty() {
            error!(error = %e, "failed to synchronize admin state from storage; aborting startup (fail-fast)");
            return Err(e.into());
        } else {
            warn!(error = %e, "warning during storage synchronization");
        }
    }

    // JwtIssuer: пустой секрет → fail-closed (503 на `/api/v3/*`, включая login, и `/api/v1/*`).
    if config.jwt_secret.is_empty() {
        warn!(
            "VKS_JWT_SECRET is empty: /api/v3/* and /api/v1/* answer 503 (fail-closed); \
             set it to enable the PNM web UI and admin API"
        );
    } else {
        admin_state.jwt_issuer = Some(Arc::new(JwtIssuer::new(JwtConfig {
            secret: config.jwt_secret.clone(),
            ttl_secs: config.jwt_ttl_secs,
            issuer: config.server_name.clone(),
        })));
        info!(ttl_secs = config.jwt_ttl_secs, "API v3 auth enabled (HS256; RS256 — долг §4.3)");
    }

    // Startup-политика оператора: bootstrap, отказ на слабой БД и разовая
    // ротация известного dev-дефолта. Вынесено в тестируемый модуль.
    let operator =
        match vks_server::operator_policy::ensure_operator(&config, &local_db, admin_state.storage.as_ref()).await {
            Ok(outcome) => outcome,
            Err(e) => {
                error!(error = %e, "operator policy rejected startup");
                std::process::exit(78);
            }
        };
    match operator {
        vks_server::operator_policy::OperatorOutcome::Registered => {
            info!(login = %config.admin_login, id = vks_core::BOOTSTRAP_ADMIN_USER_ID, "local_db operator registered")
        }
        vks_server::operator_policy::OperatorOutcome::Restored => {
            info!(login = %config.admin_login, id = vks_core::BOOTSTRAP_ADMIN_USER_ID, "operator credentials restored from persistent storage")
        }
        vks_server::operator_policy::OperatorOutcome::Rotated { login } => {
            warn!(login = %login, "rotated weak operator password from VKS_ADMIN_PASSWORD and persisted it")
        }
    }

    info!(
        allow_insecure_defaults = config.allow_insecure_defaults,
        "security policy: insecure dev defaults {}",
        if config.allow_insecure_defaults { "ENABLED (dev only!)" } else { "disabled (production)" }
    );
    if config.allow_insecure_defaults {
        warn!(
            "SECURITY: allow_insecure_defaults=true — dev defaults are accepted; never expose this listener to untrusted networks"
        );
    }

    // M1.9: AuthProvider factory (local/ldap/oidc)
    let auth_cfg = vks_auth::provider::AuthProviderConfig {
        provider_type: config.auth_provider.clone(),
        ldap_url: if config.ldap_url.is_empty() { None } else { Some(config.ldap_url.clone()) },
        ldap_bind_template: None,
        allow_mock_ldap: std::env::var("VKS_ALLOW_MOCK_LDAP").map(|v| v == "1" || v == "true").unwrap_or(false),
        oidc_issuer: None,
    };
    let auth_provider = match vks_auth::provider::create_auth_provider(&auth_cfg, Some(local_db.clone())) {
        Ok(p) => {
            info!(provider = %config.auth_provider, "auth provider initialized");
            p
        }
        Err(e) => {
            error!(error = %e, provider = %config.auth_provider, "failed to initialize configured auth provider; aborting startup");
            return Err(e.into());
        }
    };
    admin_state.auth_provider = Some(auth_provider);

    // Держим оператора в admin-хранилище users с детерминированным UserId::bootstrap_operator() и пишем в постоянное хранилище.
    if let Err(e) = vks_admin::users::Users::bootstrap_operator_async(&admin_state, &config.admin_login).await {
        if !config.storage_allow_fallback && !config.database_url.is_empty() {
            error!(error = %e, "failed to bootstrap operator into storage; aborting startup (fail-fast)");
            return Err(e.into());
        } else {
            warn!(error = %e, "failed to bootstrap operator into storage, falling back to memory");
            vks_admin::users::Users::bootstrap_operator(&admin_state, &config.admin_login);
        }
    }

    let initial_mode = match config.default_room_mode.to_lowercase().as_str() {
        "mcu" => vks_core::RoomMode::Mcu,
        _ => vks_core::RoomMode::Sfu,
    };
    let effective_mode = {
        let mut s = admin_state.settings.lock().unwrap();
        if s.name == "vks" {
            s.name = config.server_name.clone();
        }
        if s.default_mode == vks_core::RoomMode::Sfu && initial_mode != vks_core::RoomMode::Sfu {
            s.default_mode = initial_mode;
            admin_state.mode.lock().unwrap().global_mode = initial_mode;
        }
        admin_state.mode.lock().unwrap().global_mode
    };
    admin_state.webrtc_evict = Some(Arc::new(vks_server::webrtc_runtime::evict_rooms));
    let admin_state = Arc::new(admin_state);

    // 6. Загрузка статики фронтенда PNM (fail-fast диагностика, но сервер не падает)
    let frontend = Frontend::load(Path::new(&config.frontend_dir));
    if frontend.available {
        info!(root = %frontend.root.display(), "PNM frontend mounted at /pnm/ (legacy shell at /mcu/)");
    } else {
        error!(
            dir = %frontend.root.display(),
            "PNM frontend index.html not found: /pnm/ will answer 404 (set VKS_FRONTEND_DIR)"
        );
        FRONTEND_DENIED.with_label_values(&["not_configured"]).inc();
    }

    // 7. Инициализация WebRTC медиа-движков и UDP медиа-лупа (M2.1, M2.4)
    let sfu_engine = Arc::new(vks_media_sfu::SfuEngine::new());
    let mcu_engine = Arc::new(vks_media_mcu::McuEngine::new());

    let (udp_loop, receive_task, candidate_addr, candidate_port) = match vks_media_sfu::udp_loop::UdpMediaLoop::bind(
        &config.media_listen_addr,
    )
    .await
    {
        Ok(loop_handle) => {
            admin_state.set_media_healthy(true);
            let local_addr = loop_handle.local_addr();
            info!(addr = %local_addr, "WebRTC UDP media loop active");
            let sfu = sfu_engine.clone();
            let task = loop_handle.start_routing_loop(move |src, packet| sfu.route_udp_packet(src, packet.payload));
            let ip = config
                .candidate_addr
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| {
                    if local_addr.ip().is_unspecified() {
                        "127.0.0.1".into()
                    } else {
                        local_addr.ip().to_string()
                    }
                });
            let port = local_addr.port();
            info!(candidate_ip = %ip, candidate_port = %port, "WebRTC SDP ICE candidate configured");
            if ip == "127.0.0.1" && local_addr.ip().is_unspecified() {
                warn!("WebRTC candidate IP defaulted to 127.0.0.1 while listening on 0.0.0.0; external endpoints (e.g. 10.0.0.10) will fail to connect. Set candidate_addr in config or VKS_CANDIDATE_ADDR env.");
            }
            (Some(Arc::new(loop_handle)), Some(task), ip, port)
        }
        Err(e) => {
            admin_state.set_media_healthy(false);
            warn!(error = %e, addr = %config.media_listen_addr, "failed to bind UDP media loop");
            (None, None, "127.0.0.1".to_string(), 0)
        }
    };
    // Один источник истины с ICE: conference-адреса (`sip:/h323:`) — тот же
    // нормализованный candidate IP (trim/fallback/bind-fail уже учтены выше).
    let _ = admin_state.server_addr.set(candidate_addr.clone());
    if let Some(path) = vks_storage::db_lock::sqlite_path_from_url(&config.database_url) {
        // Абсолютный путь: systemd может стартовать из другого cwd, а статус
        // обязан показывать тот же файл, что открыл storage.
        let absolute = std::fs::canonicalize(&path)
            .or_else(|_| std::env::current_dir().map(|cwd| cwd.join(&path)))
            .unwrap_or(path);
        let _ = admin_state.database_file.set(absolute.to_string_lossy().into_owned());
    }

    // 8. Конфигурация обработчика сигналинга с реальными ICE/кодек атрибутами
    let ice_servers: Vec<vks_proto::signaling::IceServer> = config
        .ice_servers
        .iter()
        .map(|url| vks_proto::signaling::IceServer {
            url: url.clone(),
            username: String::new(),
            credential: String::new(),
        })
        .collect();

    let proto_room_mode = match effective_mode {
        vks_core::RoomMode::Mcu => vks_proto::media::RoomMode::Mcu as i32,
        vks_core::RoomMode::Sfu => vks_proto::media::RoomMode::Sfu as i32,
    };
    let handler_config = HandlerConfig::new(
        "srv-0001",
        &config.server_name,
        "sfuufrag",
        "sfupwd",
        "sha-256 FF:00:11",
        candidate_addr.clone(),
        candidate_port,
        vec![
            RtpMap { payload: 111, codec: "opus".into(), clock: 48000, channels: 2 },
            RtpMap { payload: 96, codec: "vp8".into(), clock: 90000, channels: 0 },
        ],
    )
    .with_ice_servers(ice_servers)
    .with_room_mode(proto_room_mode)
    // Codec caps come from the SSOT (`HandlerConfig::new` -> `webrtc_answer_specs`);
    // only the transport flag is production state. Never overwrite `RoomCaps`
    // with literals here: that silently hides new SSOT codecs from clients.
    .with_dtls_srtp(vks_server::webrtc_runtime::enabled());

    // Codec worker probe is non-fatal by default: without it the gateway keeps
    // the native profile and never advertises codecs it cannot bridge.
    if let Err(error) = vks_server::transcoder_runtime::init().await {
        config_exit(format!("transcoder required but unavailable: {error}"));
    }
    // SDP must only advertise video codecs whose bridge can actually start on
    // this host. The probe is already cached, so this is a pure lookup.
    let (h264_raw, h265_raw) = vks_server::gateways::video_bridge_availability().await;
    let availability = vks_common::codec_capabilities::VideoCodecAvailability { h264: h264_raw, h265: h265_raw };
    vks_common::codec_capabilities::set_video_codec_availability(availability);
    info!(h264 = availability.h264, h265 = availability.h265, "gateway video codec availability");
    // AAC-LATM stays unavailable until FFmpeg exposes both the AAC encoder +
    // latm muxer and the aac_latm decoder + loas demuxer. Advertising it before
    // the audio worker exists would connect calls with dead audio.
    // AAC-LATM patent/licensing sign-off is an explicit deployment decision:
    // the FFmpeg probe alone must never enable it. Operators opt in after legal
    // approval; otherwise AAC stays 488/fallback like before T06b-8.
    let accepted_codecs = vks_server::codec_policy::accepted_codecs_from_env();
    let aac_latm_legal = accepted_codecs.contains(&vks_server::codec_policy::AAC_LATM);
    if !accepted_codecs.is_empty() {
        info!(accepted_codecs = ?accepted_codecs, "operator accepted patent-encumbered codec bridges");
    }
    // Keep the three gates separate: the admin API must report the real cause
    // (`license_not_accepted` / `probe_unavailable` / `worker_pool_unavailable`)
    // instead of blaming the license for every failure (review A P2-7).
    let aac_latm_probe = vks_server::transcoder_runtime::capabilities()
        .is_some_and(vks_media_mcu::codec_worker::ffmpeg::FfmpegCapabilities::has_audio_latm_bridge);
    let aac_latm_pool = vks_server::transcoder_runtime::pool().is_some_and(|pool| pool.max_workers() >= 2);
    let aac_latm = aac_latm_legal && aac_latm_probe && aac_latm_pool;
    vks_common::codec_capabilities::set_audio_codec_availability(
        vks_common::codec_capabilities::AudioCodecAvailability {
            aac_latm_license: aac_latm_legal,
            aac_latm_probe,
            aac_latm_pool,
        },
    );
    info!(
        aac_latm,
        aac_latm_legal,
        aac_latm_probe,
        aac_latm_pool,
        "gateway audio codec availability (AAC needs probe + legal opt-in + pool preflight)"
    );

    if let Err(error) = vks_server::webrtc_runtime::preflight(&handler_config, config.allow_insecure_defaults) {
        error!(%error, "WebRTC preflight failed");
        eprintln!("vks-server: WebRTC preflight failed: {error}");
        std::process::exit(78);
    }

    let gateway_manager =
        Arc::new(GatewayManager::new().with_mcu(mcu_engine.clone()).with_candidate_ip(&candidate_addr));
    gateway_manager.bind_state(Arc::downgrade(&admin_state));
    admin_state.set_dialer(gateway_manager.clone());
    // Входящий SIP UAS — opt-in: открывать 0.0.0.0:5060 по умолчанию
    // (без auth/allowlist) нельзя. Включается только `VKS_SIP_UAS_ENABLED=1`.
    if std::env::var("VKS_SIP_UAS_ENABLED").is_ok_and(|value| value == "1") {
        let bind = std::env::var("VKS_SIP_UAS_BIND").unwrap_or_else(|_| "0.0.0.0:5060".to_string());
        let advertised = candidate_addr.clone();
        let manager = gateway_manager.clone();
        tokio::spawn(async move {
            if let Err(error) = vks_server::gateways::serve_sip_uas(manager, bind, advertised).await {
                tracing::error!(%error, "SIP UAS listener stopped");
            }
        });
    }
    // Входящий H.323 UAS: Q.931/H.245 listener 1720. По умолчанию выключен:
    // это неаутентифицированный LAN-протокол, включайте явно и ограничивайте
    // `VKS_H323_UAS_ALLOW`.
    if std::env::var("VKS_H323_UAS_ENABLED").is_ok_and(|value| value == "1") {
        let bind = std::env::var("VKS_H323_UAS_BIND").unwrap_or_else(|_| "0.0.0.0:1720".to_string());
        let local_ip = candidate_addr.clone();
        let manager = gateway_manager.clone();
        tokio::spawn(async move {
            if let Err(error) = vks_server::gateways::serve_h323_uas(manager, bind, local_ip).await {
                tracing::error!(%error, "H.323 UAS listener stopped");
            }
        });
    }
    let recorder_manager = Arc::new(RecorderManager::new());
    let cluster_manager = Arc::new(ClusterManager::new(&config.server_name));

    // Периодический heartbeat локального узла в кластере
    let cm = cluster_manager.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(5));
        loop {
            interval.tick().await;
            let now_sec =
                std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
            cm.send_heartbeat(now_sec);
        }
    });

    info!(
        server = %config.server_name,
        "SIP UAS, H.323 gateway, recorder and cluster managers initialized"
    );

    // 10. Сборка маршрутов (единая точка — vks_server::build_app_with_services)
    let app = build_app_with_services(
        admin_state,
        &frontend,
        handler_config,
        sfu_engine.clone(),
        mcu_engine.clone(),
        udp_loop.clone(),
        gateway_manager.clone(),
        recorder_manager.clone(),
        cluster_manager.clone(),
    );

    // 10. Привязка сокета и запуск сервера
    let listener = tokio::net::TcpListener::bind(&config.listen_addr).await?;
    info!(addr = %config.listen_addr, "vks-server ready and listening for HTTP/WebSocket");

    let uas_shutdown = gateway_manager.clone();
    axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>())
        .with_graceful_shutdown(async move {
            shutdown_signal().await;
            uas_shutdown.begin_shutdown();
            uas_shutdown.shutdown_sip_uas().await;
            uas_shutdown.shutdown_h323_uas().await;
        })
        .await?;

    if let Some(loop_handle) = &udp_loop {
        loop_handle.shutdown();
    }
    if let Some(task) = receive_task {
        let _ = task.await;
    }

    info!("vks-server gracefully terminated");
    Ok(())
}

/// Ожидание сигналов завершения (SIGINT / SIGTERM).
async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c().await.expect("failed to install Ctrl+C signal handler");
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut sig) => {
                sig.recv().await;
            }
            Err(e) => {
                error!(error = %e, "failed to install SIGTERM signal handler");
                std::future::pending::<()>().await;
            }
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => info!("received Ctrl+C (SIGINT), initiating shutdown"),
        _ = terminate => info!("received SIGTERM, initiating shutdown"),
    }
}

#[cfg(test)]
mod production_codec_caps_tests {
    use super::*;

    fn production_config(room_mode: vks_proto::media::RoomMode, dtls_srtp: bool) -> HandlerConfig {
        // The same builder chain production uses below (minus ICE servers).
        HandlerConfig::new(
            "srv-0001",
            "vks-server-test",
            "sfuufrag",
            "sfupwd",
            "sha-256 FF:00:11",
            "127.0.0.1",
            0,
            vec![
                RtpMap { payload: 111, codec: "opus".into(), clock: 48000, channels: 2 },
                RtpMap { payload: 96, codec: "vp8".into(), clock: 90000, channels: 0 },
            ],
        )
        .with_room_mode(room_mode as i32)
        .with_dtls_srtp(dtls_srtp)
    }

    fn expected_caps_from_ssot() -> (Vec<i32>, Vec<i32>) {
        let mut video = Vec::new();
        let mut audio = Vec::new();
        for spec in vks_common::codec_spec::webrtc_answer_specs() {
            let id = match spec.id {
                vks_common::codec_spec::CodecId::Opus => vks_proto::media::CodecId::Opus as i32,
                vks_common::codec_spec::CodecId::Vp8 => vks_proto::media::CodecId::Vp8 as i32,
                other => panic!("SSOT answer slice has an unmapped codec {other:?}"),
            };
            match spec.kind {
                vks_common::codec_spec::CodecKind::Audio => audio.push(id),
                vks_common::codec_spec::CodecKind::Video => video.push(id),
                vks_common::codec_spec::CodecKind::Dtmf => panic!("DTMF is not a RoomCaps codec"),
            }
        }
        (video, audio)
    }

    #[test]
    fn production_handler_config_caps_match_ssot_webrtc_dispositions() {
        let (video, audio) = expected_caps_from_ssot();
        let sfu = production_config(vks_proto::media::RoomMode::Sfu, true);
        let sfu_caps = sfu.caps.as_ref().expect("caps");
        assert_eq!(sfu_caps.video_codecs, video);
        assert_eq!(sfu_caps.audio_codecs, audio);
        assert_eq!(sfu_caps.room_mode, vks_proto::media::RoomMode::Sfu as i32);
        assert!(sfu_caps.dtls_srtp);

        // The MCU production path flips room_mode and the legacy DTLS flag; the
        // SSOT codec lists must survive unchanged.
        let mcu = production_config(vks_proto::media::RoomMode::Mcu, false);
        let mcu_caps = mcu.caps.as_ref().expect("caps");
        assert_eq!(mcu_caps.video_codecs, sfu_caps.video_codecs, "room_mode change dropped video caps");
        assert_eq!(mcu_caps.audio_codecs, sfu_caps.audio_codecs, "room_mode change dropped audio caps");
        assert_eq!(mcu_caps.room_mode, vks_proto::media::RoomMode::Mcu as i32);
        assert!(!mcu_caps.dtls_srtp);
    }

    #[test]
    fn production_main_does_not_overwrite_the_ssot_caps() {
        // P1 regression guard: the binary itself must not build codec literals
        // or overwrite `HandlerConfig::caps` (which is SSOT-derived).
        let source = include_str!("main.rs");
        // Split the needles so this test's own source does not match them.
        let with_caps = [".with_caps", "("].concat();
        let video_literal = ["video_codecs: ", "vec!["].concat();
        let audio_literal = ["audio_codecs: ", "vec!["].concat();
        assert!(!source.contains(&with_caps), "main must not overwrite SSOT RoomCaps");
        assert!(!source.contains(&video_literal), "main must not build video codec literals");
        assert!(!source.contains(&audio_literal), "main must not build audio codec literals");
        assert!(source.contains(".with_dtls_srtp(vks_server::webrtc_runtime::enabled())"));
    }
}
