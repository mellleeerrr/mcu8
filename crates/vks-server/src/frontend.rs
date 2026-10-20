//! Статика фронтенда PNM (сборка TrueConf MCU Web UI v2.2.1-66; рантайм ребрендирован в PROSTOR)
//! и связанные политики.
//!
//! Основание: `docs/progress/FRONTEND_PORT_PLAN.md` §4.2 (ADR-0027),
//! `docs/rules/BACKEND_RULES.md` §6 (заголовки/безопасность).
//!
//! Что здесь:
//! - [`Frontend`] — корень рантайма (`client/pnm-frontend/deployed/pnm`), `index.html` и CSP
//!   (sha256 inline-скриптов вычисляются при старте из файла — правки index.html не ломают CSP);
//! - [`Frontend::router`] — роуты статики + `Cache-Control`/`Vary: Accept-Encoding`
//!   (политики статики; подключается в `build_app`); [`Frontend::mount`] — те же роуты без слоёв;
//!   security-заголовки — app-wide слоем в `build_app` (§4.2: включая `/api/v1/*` и `/metrics`);
//! - роуты: `/` `/pnm` `/pnm/` `/pnm/{*rest}` `/mcu/{*rest}` `/admin(/)`
//!   на `tower_http::ServeDir`/`ServeFile` (эмпирика axum 0.8.9: `{*rest}` не матчит `/pnm/`,
//!   поэтому монтируем `nest_service("/pnm/")`, а `/pnm` — явный 308);
//! - [`cache_control_for_path`] — политика кэша: `immutable` только для реально хэшированных
//!   ассетов (`assets/fonts/**`, `assets/images/**`, `assets/*<hash>.webm`); всё остальное,
//!   включая `chunks/**` (правятся in-place!) и `index.html` — `no-cache`;
//! - [`with_security_headers`] — CSP + `X-Content-Type-Options`/`X-Frame-Options`/`Referrer-Policy`;
//! - [`with_cache_control`] — `Cache-Control` (+`Vary`) для путей `/pnm*`, `/mcu*`.
//!
//! Безопасность путей — на `ServeDir`: одна percent-декодировка, покомпонентная валидация
//! (`RootDir|ParentDir|Prefix` → отказ); NUL/невалидное имя → `InvalidFilename`: под `/mcu` —
//! 404, под `/pnm` — SPA-fallback (200 `index.html`). Рантайм-дерево контролируется
//! импорт-скриптом: MANIFEST.sha256, `*.bak*` исключены, **симлинки удаляются** (политика —
//! `client/pnm-frontend/NOTES.md`; `ServeDir` следует симлинкам, поэтому симлинк в дереве =
//! escape; регрессия — тест `runtime_tree_has_no_symlinks`).

use std::path::{Path, PathBuf};

use axum::extract::{RawQuery, Request};
use axum::http::{header, HeaderValue, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use base64::Engine as _;
use sha2::{Digest, Sha256};
use tower_http::services::{ServeDir, ServeFile};

use vks_common::metrics::FRONTEND_DENIED;

/// `Cache-Control` для хэшированных (иммутабельных) ассетов.
pub const CACHE_IMMUTABLE: &str = "public, max-age=31536000, immutable";
/// `Cache-Control` для не-хэшированных файлов (обязательная ревалидация; `Last-Modified`/304 от ServeDir).
pub const CACHE_NO_CACHE: &str = "no-cache";

/// Статика фронтенда: корень рантайма, index.html, CSP.
#[derive(Debug, Clone)]
pub struct Frontend {
    /// Корень рантайма (канонизированный, если каталог существует).
    pub root: PathBuf,
    /// Путь к `index.html` (SPA-точка входа и fallback).
    pub index: PathBuf,
    /// `index.html` найден и доступен.
    pub available: bool,
    /// Значение заголовка `Content-Security-Policy` (см. [`build_csp`]).
    pub csp: String,
}

impl Frontend {
    /// Загружает конфигурацию статики из каталога рантайма (наличие проверяется).
    ///
    /// При отсутствии `index.html` возвращает `available = false` (сервер продолжает
    /// работать; статика отдаёт 404, факт фиксируется логом и метрикой на уровне сборки приложения).
    pub fn load(dir: &Path) -> Self {
        let root = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
        let index = root.join("index.html");
        let html = std::fs::read_to_string(&index).ok();
        Self { root, index, available: html.is_some(), csp: build_csp(html.as_deref().unwrap_or_default()) }
    }

    /// Собирает роутер статики.
    ///
    /// - `GET /` → `307 /pnm/`;
    /// - `GET /pnm` → `308 /pnm/` (+query; axum 0.8 не редиректит сам);
    /// - `GET /pnm/` и `/pnm/{*rest}` → файл, иначе `index.html` (SPA-fallback);
    /// - `GET /pnm/administration/calibration[/]` → `308 /pnm/administration/statistics`
    ///   (раздел калибровки удалён, T1.1);
    /// - `GET /mcu` и `/mcu/{*rest}` → файл, иначе **404** (без SPA-fallback);
    /// - `GET /admin`, `/admin/` → `307 /pnm/administration` (временная ссылка шапки UI).
    pub fn mount(&self) -> Router {
        let pnm_dir = ServeDir::new(&self.root)
            .append_index_html_on_directories(false)
            .precompressed_gzip()
            .fallback(ServeFile::new(self.index.clone()).precompressed_gzip());
        let mcu_dir = ServeDir::new(&self.root).append_index_html_on_directories(false).precompressed_gzip();

        Router::new()
            .route("/", get(redirect_root))
            .route("/pnm", get(redirect_pnm))
            .route("/pnm/administration/calibration", get(redirect_removed_calibration))
            .route("/pnm/administration/calibration/", get(redirect_removed_calibration))
            .nest_service("/pnm/", pnm_dir)
            .nest_service("/mcu", mcu_dir)
            .route("/admin", get(redirect_admin))
            .route("/admin/", get(redirect_admin))
    }

    /// Роутер статики с политиками кэша: роуты [`Self::mount`] → `Cache-Control`/`Vary`.
    ///
    /// Security-заголовки сюда **не** входят: они app-wide в `build_app` (§4.2,
    /// включая `/api/v1/*` и `/metrics`) — см. `vks_server::build_app`.
    pub fn router(&self) -> Router {
        self.mount().layer(axum::middleware::from_fn(with_cache_control))
    }
}

/// Deep-link удалённого раздела калибровки (T1.1) → «Статистика».
async fn redirect_removed_calibration() -> Response {
    (StatusCode::PERMANENT_REDIRECT, [(header::LOCATION, "/pnm/administration/statistics")]).into_response()
}

async fn redirect_root() -> Response {
    (StatusCode::TEMPORARY_REDIRECT, [(header::LOCATION, "/pnm/")]).into_response()
}

/// `/pnm` → `/pnm/` (308, query сохраняется: `manifest.start_url` содержит `?homescreen=1`).
async fn redirect_pnm(RawQuery(query): RawQuery) -> Response {
    let target = match query {
        Some(query) => format!("/pnm/?{query}"),
        None => "/pnm/".to_string(),
    };
    (StatusCode::PERMANENT_REDIRECT, [(header::LOCATION, target)]).into_response()
}

/// Ссылка «Панель управления» в шапке UI ведёт на `/admin/`; до Dioxus-админки (фаза 8)
/// уводим на встроенный экран администрирования старого UI.
async fn redirect_admin() -> Response {
    (StatusCode::TEMPORARY_REDIRECT, [(header::LOCATION, "/pnm/administration")]).into_response()
}

/// Политика кэша по пути запроса (сырой путь, до strip-префикса).
///
/// `immutable` — только реально хэшированные ассеты; `chunks/**` не хэшированы
/// (имя содержит версию сборки, не content-hash) и правятся вручную — им `no-cache`.
pub fn cache_control_for_path(path: &str) -> Option<&'static str> {
    let rel = statics_rel_path(path)?;
    Some(if is_immutable_asset(rel) { CACHE_IMMUTABLE } else { CACHE_NO_CACHE })
}

/// Относительный путь внутри монтируемой статики (`/pnm…`/`/mcu…`), иначе `None`.
///
/// Требуется граница сегмента: `/pnmx` — не статика (там ничего не монтировано).
/// Единая точка для кэш-политики (здесь) и метрик (`classify_path` в `lib.rs`).
pub fn statics_rel_path(path: &str) -> Option<&str> {
    for prefix in ["/pnm", "/mcu"] {
        if let Some(rest) = path.strip_prefix(prefix) {
            if rest.is_empty() || rest.starts_with('/') {
                return Some(rest);
            }
        }
    }
    None
}

/// Хэшированный ассет (§4.2): `immutable` — только для content-hashed имён и только
/// внутри `/assets/`:
/// - `/assets/fonts/<hash>.<ext>`, `/assets/images/<hash>.<ext>` — stem ≥ 8 символов
///   `[A-Za-z0-9_-]`;
/// - `/assets/<name><hash8>.webm` — хвост из 8 символов `[A-Za-z0-9_-]`, содержащий цифру
///   (отсекает `promo_video.webm`);
/// - всё прочее (`assets/manifest.webmanifest`, `assets/images/logo.svg`, любой `.webm`
///   вне `/assets/`) — `no-cache`: имя без хэша допускает правку in-place (cache-bust §4.2).
fn is_immutable_asset(rel: &str) -> bool {
    let Some(rest) = rel.strip_prefix("/assets/") else {
        return false;
    };
    let base = rest.rsplit('/').next().unwrap_or("");
    let Some((stem, ext)) = base.rsplit_once('.') else {
        return false;
    };
    let hash_chars = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
    match ext {
        "webm" => {
            let n = stem.len();
            if n < 9 || !hash_chars(stem) {
                return false;
            }
            let tail = &stem[n - 8..];
            hash_chars(tail) && tail.bytes().any(|b| b.is_ascii_digit())
        }
        _ => (rest.starts_with("fonts/") || rest.starts_with("images/")) && hash_chars(stem) && stem.len() >= 8,
    }
}

/// Промежуточный слой: `Cache-Control` + `Vary: Accept-Encoding` для путей статики
/// и учёт денаев статики в `vks_frontend_denied_total{reason="not_found"}` (§4.7:
/// 404 под `/mcu`; traversal/invalid-name тоже приходят сюда через fallback-семантику
/// `ServeDir`, отдельного reason `traversal` у слоя нет — см. `metrics.rs`).
///
/// `Vary` обязателен при `precompressed_gzip()`: представление выбирается по
/// `Accept-Encoding`, а `ServeDir` сам этот заголовок не ставит (проверено по
/// исходникам tower-http 0.6.11). Без него общий кэш (reverse-proxy/CDN) может
/// отдать gzip-представление клиенту, который его не принимает.
pub async fn with_cache_control(request: Request, next: Next) -> Response {
    let path = request.uri().path().to_string();
    let is_static = statics_rel_path(&path).is_some();
    let mut response = next.run(request).await;
    if let Some(value) = cache_control_for_path(&path) {
        if let Ok(header_value) = HeaderValue::from_str(value) {
            response.headers_mut().insert(header::CACHE_CONTROL, header_value);
        }
        response.headers_mut().insert(header::VARY, HeaderValue::from_static("Accept-Encoding"));
    }
    if is_static && response.status() == StatusCode::NOT_FOUND {
        FRONTEND_DENIED.with_label_values(&["not_found"]).inc();
    }
    response
}

/// Промежуточный слой: заголовки безопасности (включая CSP с sha256 inline-скриптов).
///
/// HSTS не ставится здесь: только prod-TLS (см. чек-лист в плане §4.2).
pub async fn with_security_headers(csp: String, request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    if let Ok(header_value) = HeaderValue::from_str(&csp) {
        headers.insert(header::CONTENT_SECURITY_POLICY, header_value);
    }
    headers.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    headers.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("SAMEORIGIN"));
    headers.insert(header::REFERRER_POLICY, HeaderValue::from_static("same-origin"));
    response
}

/// Строит `Content-Security-Policy` для страниц UI.
///
/// `script-src` — `'self' 'unsafe-eval'` + sha256-хэши **inline**-скриптов
/// `index.html` (в сборке их пять: WS-shim, probe modern-браузера, legacy-loader
/// и два `nomodule`-скрипта; в modern-браузере исполняются три);
/// `'unsafe-inline'` в `script-src` НЕ добавляется.
///
/// `'unsafe-eval'` — вынужденная минимальная уступка неизменяемым vendor-бандлам
/// 2.2.1.66: при открытии карточки пользователя/удалении `index2.bundle` реально
/// исполняет строку как JavaScript, и без `unsafe-eval` Chromium пишет
/// «Evaluating a string as JavaScript violates … CSP» (точный текст, источник
/// `/mcu/chunks/index2.bundle-2.2.1.66.js` и разбор — в
/// `docs/progress/evidence/t21/README.md`, раздел «CSP `'unsafe-eval'`»; в
/// финальном прогоне CSP-ошибок нет, поэтому в `summary.json` сэмпла нет).
/// Статический `grep` этого пути не видит — он срабатывает в runtime.
/// Vendor-сервер `.92` CSP не отдаёт вовсе. `'unsafe-inline'` в script-src не
/// добавляется.
/// `style-src` — `'self' 'unsafe-inline'`
/// (инлайн-`<style id="prostor-pnm-overrides">` и style-атрибуты Vue).
pub fn build_csp(index_html: &str) -> String {
    let mut csp = String::from("default-src 'self'; script-src 'self' 'unsafe-eval'");
    for script in extract_inline_scripts(index_html) {
        let digest = Sha256::digest(script.as_bytes());
        let encoded = base64::engine::general_purpose::STANDARD.encode(digest);
        csp.push_str(" 'sha256-");
        csp.push_str(&encoded);
        csp.push('\'');
    }
    csp.push_str(
        "; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob:; font-src 'self' data:; \
         media-src 'self' blob:; connect-src 'self'; object-src 'none'; base-uri 'self'; \
         frame-ancestors 'self'; manifest-src 'self'; form-action 'self'",
    );
    csp
}

/// Извлекает тексты inline-`<script>` (без атрибута `src`) — их и хэшируем.
fn extract_inline_scripts(html: &str) -> Vec<&str> {
    let mut scripts = Vec::new();
    let mut rest = html;
    while let Some(start) = rest.find("<script") {
        let after_open = &rest[start..];
        let Some(tag_end) = after_open.find('>') else { break };
        let tag = &after_open[..tag_end];
        let content_start = start + tag_end + 1;
        let after = &rest[content_start..];
        let Some(close) = after.find("</script>") else { break };
        // Атрибут src именно как атрибут (не `data-src`): токенизируем тег.
        let has_src = tag.split_whitespace().any(|token| token == "src" || token.starts_with("src="));
        if !has_src {
            scripts.push(&after[..close]);
        }
        rest = &rest[content_start + close + "</script>".len()..];
    }
    scripts
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Method, Request as HttpRequest};
    use tower::ServiceExt;

    const FIXTURE_HTML: &str = concat!(
        "<!DOCTYPE html><html><head>",
        "<script>window.x=1;</script>",
        "<script type=\"module\" src=\"/mcu/chunks/app.bundle-1.0.0.js\"></script>",
        "</head><body><div id=\"root\"></div></body></html>"
    );

    /// sha256-base64 строки `window.x=1;` (единственный inline-скрипт фикстуры).
    const FIXTURE_INLINE_HASH: &str = "g7sq2RbwbLEHSBobiM09D+ctI3iVsKqmfbqIiETF4t4=";

    /// gzip-поток для `console.log(1);` (mtime=0) — для проверки precompressed_gzip.
    const GZIP_LOG_LINE: &[u8] = &[
        0x1f, 0x8b, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02, 0xff, 0x4b, 0xce, 0xcf, 0x2b, 0xce, 0xcf, 0x49, 0xd5,
        0xbc, 0x94, 0xfd, 0x73, 0x0d, 0x43, 0x6d, 0x40, 0x3d, 0x46, 0x23, 0x0f, 0x00, 0x00, 0x00,
    ];

    struct Fixture {
        base: PathBuf,
        root: PathBuf,
        /// Канорейка — **рядом** с корнем статики: `root/../canary.txt` = `base/canary.txt`.
        canary: PathBuf,
    }

    /// Чистит временный каталог теста (в т.ч. при падении — через Drop).
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.base);
        }
    }

    fn fixture(tag: &str) -> Fixture {
        let base = std::env::temp_dir().join(format!("vks-frontend-test-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);

        let root = base.join("pnm");
        std::fs::create_dir_all(root.join("chunks")).unwrap();
        std::fs::create_dir_all(root.join("assets/fonts")).unwrap();
        std::fs::create_dir_all(root.join("assets/images")).unwrap();
        std::fs::write(root.join("index.html"), FIXTURE_HTML).unwrap();
        std::fs::write(root.join("chunks/app.bundle-1.0.0.js"), "console.log(1);").unwrap();
        std::fs::write(root.join("chunks/precompressed.bundle-1.0.0.js"), "console.log(1);").unwrap();
        std::fs::write(root.join("chunks/precompressed.bundle-1.0.0.js.gz"), GZIP_LOG_LINE).unwrap();
        std::fs::write(root.join("assets/fonts/DxNvdLoR.ttf"), b"font-bytes").unwrap();
        std::fs::write(root.join("assets/images/BVqNbXmI.svg"), "<svg/>").unwrap();
        std::fs::write(root.join("assets/previewCu2j3Z3R.webm"), vec![7u8; 64]).unwrap();

        // Канорейка кладётся РЯДОМ с корнем: если защита `ServeDir` сломается, попытки
        // `../canary.txt` из корпуса реально прочитают этот файл и тест поймает утечку
        // (раньше канорейка была сиблингом base — путь до неё из корня не существовал,
        // и тест был ложно-зелёным).
        let canary = base.join("canary.txt");
        std::fs::write(&canary, "TOP-SECRET-CANARY").unwrap();
        Fixture { base, root, canary }
    }

    async fn get(app: &Router, uri: &str) -> (StatusCode, axum::http::HeaderMap, Vec<u8>) {
        let request = HttpRequest::builder().uri(uri).body(Body::empty()).unwrap();
        let response = app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let body = axum::body::to_bytes(response.into_body(), 1 << 20).await.unwrap().to_vec();
        (status, headers, body)
    }

    async fn head(app: &Router, uri: &str) -> (StatusCode, axum::http::HeaderMap) {
        let request = HttpRequest::builder().method("HEAD").uri(uri).body(Body::empty()).unwrap();
        let response = app.clone().oneshot(request).await.unwrap();
        (response.status(), response.headers().clone())
    }

    /// Как в `build_app`: роуты статики + слой Cache-Control/Vary. Security-заголовки —
    /// app-wide в `build_app`; для тестов заголовков на статике — `app_for_with_security`.
    fn app_for(fixture: &Fixture) -> Router {
        Frontend::load(&fixture.root).router()
    }

    /// `build_app`-подобная сборка для тестов security-заголовков на статике.
    fn app_for_with_security(fixture: &Fixture) -> Router {
        let frontend = Frontend::load(&fixture.root);
        let csp = frontend.csp.clone();
        frontend.router().layer(axum::middleware::from_fn(move |request: Request, next: Next| {
            let csp = csp.clone();
            async move { with_security_headers(csp, request, next).await }
        }))
    }

    #[test]
    fn csp_hashes_inline_scripts_only() {
        let csp = build_csp(FIXTURE_HTML);
        assert!(csp.contains(&format!("'sha256-{FIXTURE_INLINE_HASH}'")), "csp: {csp}");
        // Внешний скрипт (src=) не хэшируется: ровно один хэш.
        assert_eq!(csp.matches("'sha256-").count(), 1, "csp: {csp}");
        // script-src без unsafe-inline (unsafe-eval — только для vendor PNM);
        // style-src — с unsafe-inline.
        assert!(csp.contains("script-src 'self' 'unsafe-eval' 'sha256-"), "csp: {csp}");
        assert!(!csp.contains("script-src 'self' 'unsafe-inline'"), "csp: {csp}");
        assert!(csp.contains("style-src 'self' 'unsafe-inline'"), "csp: {csp}");
        assert!(csp.contains("object-src 'none'"), "csp: {csp}");
    }

    #[test]
    fn csp_ignores_data_src_attribute() {
        // `data-src` не должен считаться внешним src (иначе vite-legacy-entry не хэшируется).
        let html = "<script nomodule id=\"e\" data-src=\"/mcu/x.js\">System.import(1);</script>";
        let csp = build_csp(html);
        assert_eq!(csp.matches("'sha256-").count(), 1, "csp: {csp}");
    }

    #[test]
    fn cache_policy_matrix() {
        assert_eq!(cache_control_for_path("/pnm/chunks/app.bundle-1.0.0.js"), Some(CACHE_NO_CACHE));
        assert_eq!(cache_control_for_path("/mcu/chunks/index.bundle-2.2.1.66.js"), Some(CACHE_NO_CACHE));
        assert_eq!(cache_control_for_path("/pnm/"), Some(CACHE_NO_CACHE));
        assert_eq!(cache_control_for_path("/pnm/administration/security"), Some(CACHE_NO_CACHE));
        assert_eq!(cache_control_for_path("/mcu/assets/fonts/DxNvdLoR.ttf"), Some(CACHE_IMMUTABLE));
        assert_eq!(cache_control_for_path("/mcu/assets/images/BVqNbXmI.svg"), Some(CACHE_IMMUTABLE));
        assert_eq!(cache_control_for_path("/mcu/assets/previewCu2j3Z3R.webm"), Some(CACHE_IMMUTABLE));
        assert_eq!(cache_control_for_path("/mcu/assets/auto_scroll8oj5ttnk.webm"), Some(CACHE_IMMUTABLE));
        // Хэш-эвристика (§4.2): не-хэшированные имена под /assets → no-cache.
        assert_eq!(cache_control_for_path("/mcu/assets/images/logo.svg"), Some(CACHE_NO_CACHE));
        assert_eq!(cache_control_for_path("/mcu/assets/fonts/font.ttf"), Some(CACHE_NO_CACHE));
        assert_eq!(cache_control_for_path("/mcu/assets/video/promo_video.webm"), Some(CACHE_NO_CACHE));
        assert_eq!(cache_control_for_path("/mcu/chunks/promo_video.webm"), Some(CACHE_NO_CACHE));
        assert_eq!(cache_control_for_path("/mcu/assets/manifest.webmanifest"), Some(CACHE_NO_CACHE));
        assert_eq!(cache_control_for_path("/mcu/assets/browserconfig.xml"), Some(CACHE_NO_CACHE));
        assert_eq!(cache_control_for_path("/api/v1/rooms"), None);
        assert_eq!(cache_control_for_path("/metrics"), None);
        // Граница сегмента: `/pnmx` — не монтированная статика.
        assert_eq!(cache_control_for_path("/pnmx"), None);
    }

    #[tokio::test]
    async fn root_and_admin_redirects() {
        let f = fixture("redirects");
        let app = app_for(&f);
        let (status, headers, _) = get(&app, "/").await;
        assert_eq!(status, StatusCode::TEMPORARY_REDIRECT);
        assert_eq!(headers.get(header::LOCATION).unwrap().to_str().unwrap(), "/pnm/");

        let (status, headers, _) = get(&app, "/admin").await;
        assert_eq!(status, StatusCode::TEMPORARY_REDIRECT);
        assert_eq!(headers.get(header::LOCATION).unwrap().to_str().unwrap(), "/pnm/administration");
        let (status, headers, _) = get(&app, "/admin/").await;
        assert_eq!(status, StatusCode::TEMPORARY_REDIRECT);
        assert_eq!(headers.get(header::LOCATION).unwrap().to_str().unwrap(), "/pnm/administration");
    }

    #[tokio::test]
    async fn removed_calibration_redirects_to_statistics() {
        let f = fixture("calibration-redirect");
        let app = app_for(&f);
        for uri in [
            "/pnm/administration/calibration",
            "/pnm/administration/calibration/",
            "/pnm/administration/calibration?from=bookmark",
        ] {
            let (status, headers, _) = get(&app, uri).await;
            assert_eq!(status, StatusCode::PERMANENT_REDIRECT, "{uri}");
            assert_eq!(
                headers.get(header::LOCATION).unwrap().to_str().unwrap(),
                "/pnm/administration/statistics",
                "{uri}"
            );
            // Редирект идёт через слой кэша: политика `/pnm/*` — no-cache.
            assert_eq!(headers.get(header::CACHE_CONTROL).unwrap().to_str().unwrap(), CACHE_NO_CACHE, "{uri}");
        }
        // HEAD обслуживается тем же GET-роутом (axum срезает тело).
        let (status, headers) = head(&app, "/pnm/administration/calibration").await;
        assert_eq!(status, StatusCode::PERMANENT_REDIRECT);
        assert_eq!(headers.get(header::LOCATION).unwrap().to_str().unwrap(), "/pnm/administration/statistics");

        // Соседние страницы админки отдаются как обычно (SPA-fallback).
        let (status, _, _) = get(&app, "/pnm/administration/statistics").await;
        assert_eq!(status, StatusCode::OK);
    }

    #[tokio::test]
    async fn pnm_without_slash_redirects_preserving_query() {
        let f = fixture("slash");
        let app = app_for(&f);
        let (status, headers, _) = get(&app, "/pnm").await;
        assert_eq!(status, StatusCode::PERMANENT_REDIRECT);
        assert_eq!(headers.get(header::LOCATION).unwrap().to_str().unwrap(), "/pnm/");
        let (status, headers, _) = get(&app, "/pnm?homescreen=1&x=2").await;
        assert_eq!(status, StatusCode::PERMANENT_REDIRECT);
        assert_eq!(headers.get(header::LOCATION).unwrap().to_str().unwrap(), "/pnm/?homescreen=1&x=2");
    }

    #[tokio::test]
    async fn pnm_index_and_spa_fallback() {
        let f = fixture("spa");
        let app = app_for(&f);
        for uri in ["/pnm/", "/pnm/authorization", "/pnm/administration/security"] {
            let (status, headers, body) = get(&app, uri).await;
            assert_eq!(status, StatusCode::OK, "{uri}");
            assert_eq!(headers.get(header::CACHE_CONTROL).unwrap().to_str().unwrap(), CACHE_NO_CACHE);
            assert_eq!(headers.get(header::VARY).unwrap().to_str().unwrap(), "Accept-Encoding", "{uri}");
            assert!(String::from_utf8_lossy(&body).contains("<div id=\"root\">"), "{uri}");
        }
    }

    #[tokio::test]
    async fn chunks_are_no_cache_and_typed() {
        let f = fixture("chunks");
        let app = app_for(&f);
        let (status, headers, body) = get(&app, "/pnm/chunks/app.bundle-1.0.0.js").await;
        assert_eq!(status, StatusCode::OK);
        assert!(headers.get(header::CONTENT_TYPE).unwrap().to_str().unwrap().starts_with("text/javascript"));
        assert_eq!(headers.get(header::CACHE_CONTROL).unwrap().to_str().unwrap(), CACHE_NO_CACHE);
        assert_eq!(headers.get(header::VARY).unwrap().to_str().unwrap(), "Accept-Encoding");
        assert_eq!(body, b"console.log(1);");
    }

    #[tokio::test]
    async fn hashed_assets_are_immutable() {
        let f = fixture("immutable");
        let app = app_for(&f);
        for uri in ["/mcu/assets/fonts/DxNvdLoR.ttf", "/pnm/assets/images/BVqNbXmI.svg"] {
            let (status, headers, _) = get(&app, uri).await;
            assert_eq!(status, StatusCode::OK, "{uri}");
            assert_eq!(headers.get(header::CACHE_CONTROL).unwrap().to_str().unwrap(), CACHE_IMMUTABLE);
            assert_eq!(headers.get(header::VARY).unwrap().to_str().unwrap(), "Accept-Encoding");
        }
    }

    #[tokio::test]
    async fn webm_range_request_206() {
        let f = fixture("range");
        let app = app_for(&f);
        let request = HttpRequest::builder()
            .uri("/mcu/assets/previewCu2j3Z3R.webm")
            .header(header::RANGE, "bytes=0-9")
            .body(Body::empty())
            .unwrap();
        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(response.headers().get(header::CONTENT_RANGE).unwrap().to_str().unwrap(), "bytes 0-9/64");
        let body = axum::body::to_bytes(response.into_body(), 1 << 20).await.unwrap();
        assert_eq!(body.len(), 10);
    }

    #[tokio::test]
    async fn mcu_missing_and_bare_are_404() {
        let f = fixture("mcu404");
        let app = app_for(&f);
        let denied = FRONTEND_DENIED.with_label_values(&["not_found"]);
        let before = denied.get();
        for uri in ["/mcu/", "/mcu", "/mcu/chunks/nope.js", "/mcu/assets/fonts/nope.ttf"] {
            let (status, _, body) = get(&app, uri).await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{uri}");
            assert!(!String::from_utf8_lossy(&body).contains("<div id=\"root\">"), "no SPA fallback for {uri}");
        }
        assert!(denied.get() > before, "vks_frontend_denied_total{{reason=not_found}} must increment on misses");
        // Существующий файл под /mcu отдаётся.
        let (status, _, _) = get(&app, "/mcu/chunks/app.bundle-1.0.0.js").await;
        assert_eq!(status, StatusCode::OK);
    }

    #[tokio::test]
    async fn head_and_if_modified_since() {
        let f = fixture("head");
        let app = app_for(&f);
        let request = HttpRequest::builder().method(Method::HEAD).uri("/pnm/").body(Body::empty()).unwrap();
        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert!(response.headers().get(header::CONTENT_LENGTH).is_some());
        let body = axum::body::to_bytes(response.into_body(), 1 << 20).await.unwrap();
        assert!(body.is_empty(), "HEAD must have empty body");

        // Получаем Last-Modified и повторяем с If-Modified-Since → 304.
        let (_, headers, _) = get(&app, "/pnm/chunks/app.bundle-1.0.0.js").await;
        let last_modified = headers.get(header::LAST_MODIFIED).unwrap().clone();
        let request = HttpRequest::builder()
            .uri("/pnm/chunks/app.bundle-1.0.0.js")
            .header(header::IF_MODIFIED_SINCE, last_modified)
            .body(Body::empty())
            .unwrap();
        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::NOT_MODIFIED);
    }

    #[tokio::test]
    async fn precompressed_gzip_is_served() {
        let f = fixture("gzip");
        let app = app_for(&f);
        let request = HttpRequest::builder()
            .uri("/mcu/chunks/precompressed.bundle-1.0.0.js")
            .header(header::ACCEPT_ENCODING, "gzip")
            .body(Body::empty())
            .unwrap();
        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers().get(header::CONTENT_ENCODING).unwrap().to_str().unwrap(), "gzip");
        let body = axum::body::to_bytes(response.into_body(), 1 << 20).await.unwrap();
        assert_eq!(body.as_ref(), GZIP_LOG_LINE);
    }

    #[tokio::test]
    async fn traversal_corpus_cannot_escape_root() {
        let f = fixture("traversal");
        let app = app_for(&f);
        let canary_name = f.canary.file_name().unwrap().to_str().unwrap().to_string();
        let attempts = [
            format!("/pnm/../{canary_name}"),
            format!("/pnm/%2e%2e/{canary_name}"),
            format!("/pnm/%2E%2E%2F{canary_name}"),
            format!("/pnm/..%2f{canary_name}"),
            format!("/pnm/..%5c..%5c{canary_name}"),
            format!("/pnm/%252e%252e/{canary_name}"),
            format!("/pnm/%2f..%2f{canary_name}"),
            format!("/pnm/chunks/../../{canary_name}"),
            format!("/pnm/%2e%2e%2f{canary_name}"),
            format!("/pnm/%c0%ae%c0%ae/{canary_name}"),
            format!("/pnm/%00../../{canary_name}"),
            format!("/pnm/%FF/{canary_name}"),
            format!("/mcu/../{canary_name}"),
            format!("/mcu/%2e%2e/{canary_name}"),
            format!("/mcu/..%2f{canary_name}"),
            format!("/mcu/%252e%252e%2f{canary_name}"),
            format!("/mcu/chunks/../../{canary_name}"),
        ];
        for uri in attempts {
            let (status, _, body) = get(&app, &uri).await;
            let text = String::from_utf8_lossy(&body);
            assert!(!text.contains("TOP-SECRET-CANARY"), "{uri} leaked canary (status {status})");
            assert!(status == StatusCode::OK || status == StatusCode::NOT_FOUND, "{uri}: unexpected status {status}");
        }
        // Контроль живости корпуса: сама канорейка существует и читается напрямую —
        // иначе тест снова выродится в ложно-зелёный.
        assert_eq!(std::fs::read_to_string(&f.canary).unwrap(), "TOP-SECRET-CANARY");
    }

    /// Политика симлинков (NOTES.md): `ServeDir` **следует** симлинкам, поэтому рантайм-дерево
    /// обязано быть без них — импорт-скрипт их удаляет, а этот тест сторожит инвариант.
    #[test]
    fn runtime_tree_has_no_symlinks() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../client/pnm-frontend/deployed/pnm");
        if !dir.is_dir() {
            eprintln!("skip: runtime tree not present at {}", dir.display());
            return;
        }
        let mut symlinks: Vec<PathBuf> = Vec::new();
        let mut files = 0usize;
        let mut stack = vec![dir.clone()];
        while let Some(cur) = stack.pop() {
            for entry in std::fs::read_dir(&cur).expect("read_dir runtime tree") {
                let entry = entry.expect("dir entry");
                let file_type = entry.file_type().expect("file type");
                if file_type.is_symlink() {
                    symlinks.push(entry.path());
                } else if file_type.is_dir() {
                    stack.push(entry.path());
                } else {
                    files += 1;
                }
            }
        }
        assert!(files > 0, "runtime tree looks empty: {}", dir.display());
        assert!(symlinks.is_empty(), "symlinks in runtime tree (policy: NOTES.md): {symlinks:?}");
    }

    #[tokio::test]
    async fn post_is_405_not_fallback() {
        let f = fixture("post405");
        let app = app_for(&f);
        let request = HttpRequest::builder().method(Method::POST).uri("/pnm/x").body(Body::empty()).unwrap();
        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
    }

    #[tokio::test]
    async fn security_headers_layer_sets_csp_and_friends() {
        let f = fixture("headers");
        let frontend = Frontend::load(&f.root);
        assert!(frontend.available);
        assert!(frontend.csp.contains(FIXTURE_INLINE_HASH), "csp: {}", frontend.csp);
        // Security-заголовки живут app-wide в `build_app`, `Frontend::router` их не ставит
        // (см. `router_alone_does_not_set_security_headers`) — тест берёт сборку-аналог.
        let app = app_for_with_security(&f);
        let (status, headers, _) = get(&app, "/pnm/").await;
        assert_eq!(status, StatusCode::OK);
        let csp = headers.get(header::CONTENT_SECURITY_POLICY).unwrap().to_str().unwrap();
        assert!(csp.contains(&format!("'sha256-{FIXTURE_INLINE_HASH}'")), "csp: {csp}");
        assert!(csp.contains("script-src 'self' 'unsafe-eval' 'sha256-"), "csp: {csp}");
        assert_eq!(headers.get(header::X_CONTENT_TYPE_OPTIONS).unwrap().to_str().unwrap(), "nosniff");
        assert_eq!(headers.get(header::X_FRAME_OPTIONS).unwrap().to_str().unwrap(), "SAMEORIGIN");
        assert_eq!(headers.get(header::REFERRER_POLICY).unwrap().to_str().unwrap(), "same-origin");
    }

    /// Security-заголовки — только app-wide (`build_app`); `Frontend::router` их не ставит,
    /// чтобы слой не накладывался дважды (аудит: тест `security_headers_layer_sets_csp_and_friends`
    /// раньше опирался на слой внутри `router`).
    #[tokio::test]
    async fn router_alone_does_not_set_security_headers() {
        let f = fixture("noheaders");
        let app = app_for(&f);
        let (status, headers, _) = get(&app, "/pnm/").await;
        assert_eq!(status, StatusCode::OK);
        assert!(headers.get(header::CONTENT_SECURITY_POLICY).is_none());
        assert!(headers.get(header::X_CONTENT_TYPE_OPTIONS).is_none());
        assert!(headers.get(header::X_FRAME_OPTIONS).is_none());
        assert!(headers.get(header::REFERRER_POLICY).is_none());
    }

    #[tokio::test]
    async fn unavailable_frontend_serves_404_but_redirects_work() {
        let missing = std::env::temp_dir().join(format!("vks-frontend-missing-{}", std::process::id()));
        let frontend = Frontend::load(&missing);
        assert!(!frontend.available);
        let app = frontend.router();
        let (status, _, _) = get(&app, "/pnm/").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        let (status, _, _) = get(&app, "/").await;
        assert_eq!(status, StatusCode::TEMPORARY_REDIRECT);
    }
}
