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

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 1. Инициализация трейсинга
    init_tracing(None, LogFormat::Human);
    info!("vks-server starting up...");

    // 2. Инициализация метрик Prometheus
    init_metrics();

    // 3. Загрузка конфигурации
    let config_path = std::env::args().nth(1).or_else(|| std::env::var("VKS_CONFIG_PATH").ok()).or_else(|| {
        if std::path::Path::new("configs/default.toml").exists() {
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
                error!(error = %e, path = ?config_path, "failed to load configured config file; aborting startup (fail-fast)");
                return Err(e.into());
            } else {
                warn!(error = %e, "failed to load config file, falling back to defaults");
                Config::load(None).unwrap_or_default()
            }
        }
    };

    // M0.3: Валидация конфигурации перед продолжением старта
    config.validate()?;

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

    // LocalDb-оператор (dev-дефолт admin/admin; прод — VKS_ADMIN_PASSWORD).
    if local_db.user(vks_core::BOOTSTRAP_ADMIN_USER_ID).is_err() {
        match local_db.register(vks_core::BOOTSTRAP_ADMIN_USER_ID, &config.admin_login, &config.admin_password) {
            Ok(_) => {
                info!(login = %config.admin_login, id = vks_core::BOOTSTRAP_ADMIN_USER_ID, "local_db operator registered")
            }
            Err(e) => warn!(error = %e, "local_db operator bootstrap skipped"),
        }
    } else {
        info!(login = %config.admin_login, id = vks_core::BOOTSTRAP_ADMIN_USER_ID, "operator credentials restored from persistent storage");
    }
    if config.admin_password == "admin" {
        warn!("admin password is the dev default; set VKS_ADMIN_PASSWORD before production");
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
                warn!("WebRTC candidate IP defaulted to 127.0.0.1 while listening on 0.0.0.0; external endpoints (e.g. 192.168.1.132) will fail to connect. Set candidate_addr in config or VKS_CANDIDATE_ADDR env.");
            }
            (Some(Arc::new(loop_handle)), Some(task), ip, port)
        }
        Err(e) => {
            admin_state.set_media_healthy(false);
            warn!(error = %e, addr = %config.media_listen_addr, "failed to bind UDP media loop");
            (None, None, "127.0.0.1".to_string(), 0)
        }
    };

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
    let caps = vks_proto::media::RoomCaps {
        room_mode: proto_room_mode,
        video_codecs: vec![vks_proto::media::CodecId::Vp8 as i32, vks_proto::media::CodecId::H264 as i32],
        audio_codecs: vec![vks_proto::media::CodecId::Opus as i32],
        simulcast: true,
        svc: false,
        dtls_srtp: true,
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
    .with_caps(caps);

    let gateway_manager =
        Arc::new(GatewayManager::new().with_mcu(mcu_engine.clone()).with_candidate_ip(&candidate_addr));
    gateway_manager.bind_state(Arc::downgrade(&admin_state));
    admin_state.set_dialer(gateway_manager.clone());
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

    axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>())
        .with_graceful_shutdown(shutdown_signal())
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
