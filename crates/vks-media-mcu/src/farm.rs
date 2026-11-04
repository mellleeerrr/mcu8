//! `O7`: ферма энкодеров — один H.264/H.265-энкодер на группу зрителей
//! с идентичным выходным профилем (спека `07_reference_transplant_spec.md` §B).
//!
//! ## Зачем
//! Сегодня каждая egress-сессия держит свой FFmpeg-энкодер и отдаёт в него
//! один и тот же композит: 20 зрителей комнаты = 20 энкодеров на одном CPU
//! (0.28–0.31 ядра каждый, `[ЗАМЕР]` Lead). Битстрим у них побитово одинаков —
//! делит его только RTP-обвязка (свой SSRC/seq/ts на зрителя, F10).
//!
//! ## Что здесь есть
//! - [`EncoderKey`] — полный набор параметров, от которых зависит битстрим;
//!   разные ключи → разные энкодеры (разные профили/caps — законная деградация).
//! - [`EncoderFarm`] — `acquire`/`publish`/`release`/`invalidate_room`/`snapshot`
//!   с refcount = зрители + сам энкодер (спека §B.4–B.5).
//! - Один актор на ключ: пишет кадры в stdin child-процесса, читает Annex-B,
//!   собирает access units (AUD = граница), кэширует SPS/PPS, рассылает
//!   `AccessUnit` в `tokio::sync::broadcast` (нулевые копии NAL: `Arc<[u8]>`).
//! - Групповой счётчик `keyframe_requests` (PLI/FIR одного зрителя → запрос
//!   IDR группы; в v1 keyint = fps, поэтому форс-IDR не нужен — см. ниже).
//!
//! ## Границы (осознанно)
//! - **Self-view несовместим с фермой** (ADR-0029): разным зрителям нужен
//!   разный кадр — такой зритель идёт старым per-session путём. Ферма — только
//!   комнаты без self-view (по умолчанию выключен).
//! - **Форс-IDR не делается**: у raw-pipe FFmpeg нет runtime-хука; рестарт
//!   child'а рвёт поток всем зрителям (спека §B.6), а `keyint = fps` уже даёт
//!   восстановление ≤ 1 с. Счётчик потребляется, запросы видны в метриках;
//!   принудительный IDR — задача P4b (нужен control-канал энкодера).
//! - **`-vf scale` в FFmpeg** вместо resize в parent: включается, когда холст
//!   комнаты не совпал с выходным профилем (O1 делает их равными для первой
//!   сессии, поэтому обычно масштабирования нет вовсе).
//!
//! ## Инварианты
//! - Refcount групп монотонно уменьшается только через `EncoderFarm::release`
//!   (или `Drop` у [`GroupViewer`]); при нуле child убивается группой процессов
//!   (ADR-0028: `kill(-pgid, SIGKILL)`), чтобы не осталось осиротевших FFmpeg.
//! - `publish` не блокирует вызывающего: очередь в группу ёмкостью 1, лишнее
//!   отбрасывается (дедуп по `min_interval`, N сессий публикуют один композит).
//! - Отсутствие кадров не фатально: следующий `publish` продолжает поток.

use std::collections::BTreeMap;
use std::io;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use std::sync::atomic::AtomicU32;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::Child;
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;

use crate::frame::Frame;
use vks_core::RoomId;

/// Кодек выхода фермы. Видео-энкодер всегда один из двух (v1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FarmCodec {
    /// H.264 (libx264, `-f h264`).
    H264,
    /// H.265 (libx265, `-f hevc`).
    H265,
}

impl FarmCodec {
    /// Короткое имя кодека для меток метрик (`h264`/`h265`).
    pub const fn name(self) -> &'static str {
        match self {
            Self::H264 => "h264",
            Self::H265 => "h265",
        }
    }

    /// Имя кодека для FFmpeg (`-c:v`).
    pub const fn encoder(self) -> &'static str {
        match self {
            Self::H264 => "libx264",
            Self::H265 => "libx265",
        }
    }

    /// Muxer элементарного потока (`-f`).
    pub const fn muxer(self) -> &'static str {
        match self {
            Self::H264 => "h264",
            Self::H265 => "hevc",
        }
    }

    /// Параметр FFmpeg для тонкой настройки энкодера.
    const fn params_flag(self) -> &'static str {
        match self {
            Self::H264 => "-x264-params",
            Self::H265 => "-x265-params",
        }
    }

    /// Канонические параметры энкодера по умолчанию.
    pub const fn default_params(self) -> &'static str {
        match self {
            Self::H264 => {
                "repeat-headers=1:aud=1:scenecut=0:bframes=0:cabac=0:weightp=0:8x8dct=0:ref=1:rc-lookahead=0:slice-max-size=1180:deblock=0,0"
            }
            Self::H265 => {
                "log-level=error:bframes=0:rc-lookahead=0:scenecut=0:sao=0:wpp=1:frame-threads=1:pools=1:aud=1:repeat-headers=1"
            }
        }
    }

    /// Параметры энкодера для конкретного профиля / контента.
    pub fn params_for_profile(self, profile: &str, width: u32, _height: u32, fps: u32) -> &'static str {
        match self {
            Self::H264 => {
                if profile.eq_ignore_ascii_case("high") {
                    "repeat-headers=1:aud=1:scenecut=0:bframes=0:cabac=1:8x8dct=1:aq-mode=2:ref=2:rc-lookahead=0:slice-max-size=1180"
                } else if fps <= 15 && width >= 1920 {
                    "repeat-headers=1:aud=1:scenecut=0:bframes=0:aq-mode=3:slice-max-size=1180:rc-lookahead=0"
                } else {
                    "repeat-headers=1:aud=1:scenecut=0:bframes=0:cabac=0:weightp=0:8x8dct=0:ref=1:rc-lookahead=0:slice-max-size=1180:deblock=0,0"
                }
            }
            Self::H265 => {
                "log-level=error:bframes=0:rc-lookahead=0:scenecut=0:sao=0:wpp=1:frame-threads=1:pools=1:aud=1:repeat-headers=1"
            }
        }
    }

    /// Является ли NAL IDR-кадром (ключевым).
    fn is_idr_nal(self, nal: &[u8]) -> bool {
        let Some((_, start)) = find_start_code(nal, 0) else { return false };
        let Some(&header) = nal.get(start) else { return false };
        match self {
            // H.264: nal_unit_type 5 = IDR.
            Self::H264 => header & 0x1f == 5,
            // H.265: nal_unit_type 16..=21 (BLA/IDR/CRA).
            Self::H265 => (header >> 1) & 0x3f >= 16 && (header >> 1) & 0x3f <= 21,
        }
    }

    /// Является ли NAL параметрами потока (SPS/PPS/VPS).
    fn is_parameter_set(self, nal: &[u8]) -> bool {
        let Some((_, start)) = find_start_code(nal, 0) else { return false };
        let Some(&header) = nal.get(start) else { return false };
        match self {
            // H.264: 7 = SPS, 8 = PPS.
            Self::H264 => matches!(header & 0x1f, 7 | 8),
            // H.265: 32 = VPS, 33 = SPS, 34 = PPS.
            Self::H265 => (32..=34).contains(&((header >> 1) & 0x3f)),
        }
    }
}

/// Выходной профиль группы: всё, от чего зависит битстрим.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EncoderSpec {
    /// Кодек выхода.
    pub codec: FarmCodec,
    /// Ширина выхода, px.
    pub width: u32,
    /// Высота выхода, px.
    pub height: u32,
    /// Кадров в секунду (вход и выход).
    pub fps: u32,
    /// Целевой битрейт, кбит/с.
    pub bitrate_kbps: u32,
    /// Максимальный битрейт, кбит/с.
    pub maxrate_kbps: u32,
    /// Размер буфера VBV, кбит.
    pub bufsize_kbps: u32,
    /// H.264 profile (`baseline`/`main`/`high`).
    pub profile: String,
    /// H.264 level (`3.0`/`3.1`/`4.0`).
    pub level: String,
    /// Интервал IDR в кадрах.
    pub keyint: u32,
    /// Строка параметров энкодера (`-x264-params`/`-x265-params`), как её
    /// задаёт шлюз: профиль/level и эти параметры вместе определяют битстрим.
    pub encoder_params: String,
}

impl EncoderSpec {
    /// Единый канонический конструктор профиля группы фермы.
    pub fn canonical(
        codec: FarmCodec,
        width: u32,
        height: u32,
        fps: u32,
        bitrate_kbps: u32,
        profile: impl Into<String>,
        level: impl Into<String>,
    ) -> Self {
        let fps = fps.clamp(1, 60);
        let bitrate_kbps = bitrate_kbps.max(64);
        let profile_str = profile.into().trim().to_lowercase();
        let level_str = level.into().trim().to_string();
        let width = (width.max(160) / 2) * 2;
        let height = (height.max(120) / 2) * 2;
        let encoder_params = codec.params_for_profile(&profile_str, width, height, fps).to_string();
        Self {
            codec,
            width,
            height,
            fps,
            bitrate_kbps,
            maxrate_kbps: (bitrate_kbps * 115) / 100,
            bufsize_kbps: (bitrate_kbps / 2).max(64),
            profile: profile_str,
            level: level_str,
            keyint: (fps * 2).clamp(15, 60),
            encoder_params,
        }
    }

    /// Длительность кадра группы.
    pub fn frame_interval(&self) -> Duration {
        Duration::from_micros(1_000_000 / u64::from(self.fps.max(1)))
    }

    /// Ключ группы: профиль + версия холста + комната.
    pub fn key(&self, room: RoomId, canvas_version: u64) -> EncoderKey {
        EncoderKey { room, canvas_version, spec: self.clone() }
    }
}

/// `P4a`: агрегированная RTP-статистика **зрителей** группы.
///
/// Один экземпляр на группу (агрегат по всем её зрителям): пакеты/байты,
/// отправленные per-viewer RTP-обвязчиком, и кадры, потерянные конкретными
/// зрителями (`broadcast::Lagged`, IDR-гейт). Сам энкодер сюда не входит —
/// его счётчики в [`GroupStat`]. Метрики: `viewer_rtp_*`.
#[derive(Debug, Default)]
pub struct ViewerRtp {
    packets: AtomicU64,
    bytes: AtomicU64,
    dropped_frames: AtomicU64,
}

impl ViewerRtp {
    /// Сколько RTP-пакетов отправлено зрителям группы.
    #[must_use]
    pub fn packets(&self) -> u64 {
        self.packets.load(Ordering::Relaxed)
    }

    /// Сколько байт payload отправлено зрителям группы.
    #[must_use]
    pub fn bytes(&self) -> u64 {
        self.bytes.load(Ordering::Relaxed)
    }

    /// Сколько кадров потеряно на стороне зрителей (лаг/IDR-гейт).
    #[must_use]
    pub fn dropped_frames(&self) -> u64 {
        self.dropped_frames.load(Ordering::Relaxed)
    }

    /// Учитывает успешную отправку AU: `packets` RTP-пакетов, `bytes` payload.
    pub fn note_sent(&self, packets: u64, bytes: u64) {
        self.packets.fetch_add(packets, Ordering::Relaxed);
        self.bytes.fetch_add(bytes, Ordering::Relaxed);
    }

    /// Учитывает потерю кадров на стороне зрителя.
    pub fn note_dropped(&self, frames: u64) {
        self.dropped_frames.fetch_add(frames, Ordering::Relaxed);
    }

    /// Текущий снимок — для метрик/админки: (packets, bytes, dropped).
    #[must_use]
    pub fn snapshot(&self) -> (u64, u64, u64) {
        (self.packets(), self.bytes(), self.dropped_frames())
    }
}

/// Полный ключ группы: разные ключи → разные энкодеры.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EncoderKey {
    /// Комната: раскладка и кадры разные у разных комнат.
    pub room: RoomId,
    /// Монотонная версия холста/раскладки комнаты (инвалидация при смене O1-холста).
    pub canvas_version: u64,
    /// Выходной профиль.
    pub spec: EncoderSpec,
}

/// Один закодированный access unit (битстрим без RTP-обвязки).
#[derive(Debug, Clone)]
pub struct AccessUnit {
    /// Annex-B байты AU (без AUD, NAL'ы с start code).
    data: Arc<[u8]>,
    /// Содержит IDR-кадр (ключевой): нужен новому зрителю как точка входа.
    is_idr: bool,
    /// Момент кодирования, мкс от старта группы (для диагностики).
    pts_us: u64,
    /// SPS/PPS/VPS, встреченные в этом AU (обновляют кэш группы).
    parameter_sets: Vec<Arc<[u8]>>,
}

impl AccessUnit {
    /// Annex-B байты AU.
    pub fn data(&self) -> &[u8] {
        &self.data
    }

    /// Есть ли в AU IDR-кадр.
    pub fn is_idr(&self) -> bool {
        self.is_idr
    }

    /// PTS в мкс от старта группы.
    pub fn pts_us(&self) -> u64 {
        self.pts_us
    }

    /// SPS/PPS/VPS этого AU.
    pub fn parameter_sets(&self) -> &[Arc<[u8]>] {
        &self.parameter_sets
    }
}

/// Ошибки фермы (fail-closed: зритель получает отказ, а не «тихое» видео).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FarmError {
    /// Лимит числа групп исчерпан (M12): вызывающий отдаёт 488/503.
    #[error("encoder farm is at capacity ({limit} groups)")]
    Overloaded {
        /// Текущий лимит групп.
        limit: usize,
    },
    /// FFmpeg не поднялся (нет бинаря/кодека).
    #[error("encoder process could not be started: {0}")]
    Spawn(String),
    /// Группа уже остановлена (инвалидация холста/комнаты).
    #[error("encoder group is no longer accepting viewers")]
    Closed,
}

/// Soak budget: a released group may leak at most this much parent RSS.
pub const FARM_GROUP_LEAK_BUDGET_BYTES: u64 = 1024 * 1024;
/// Soak budget: a live group may retain at most this much parent RSS.
pub const FARM_GROUP_RETENTION_BUDGET_BYTES: u64 = 12 * 1024 * 1024;
/// Group count used by the T09a release soak (66 sequential start/stops).
pub const FARM_SOAK_GROUPS: u64 = 66;

/// Total leak/retention budget for `groups` soak cycles (allocation-free
/// arithmetic so the CI gate is deterministic across allocators).
pub fn farm_soak_budget(groups: u64) -> (u64, u64) {
    (groups.saturating_mul(FARM_GROUP_LEAK_BUDGET_BYTES), groups.saturating_mul(FARM_GROUP_RETENTION_BUDGET_BYTES))
}

/// Диагностика одной группы для `/metrics` и `/api/v3`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupStat {
    /// Ключ группы.
    pub key: EncoderKey,
    /// Зрителей + сам энкодер (refcount).
    pub refcount: usize,
    /// Сколько AU закодировано.
    pub frames: u64,
    /// Сколько AU было ключевыми.
    pub keyframes: u64,
    /// Сколько публикаций отброшено (дедуп/переполнение).
    pub dropped_frames: u64,
    /// Рестартов child-процесса.
    pub restarts: u64,
    /// Всего запросов keyframe (PLI/FIR).
    pub keyframe_requests: u64,
    /// Зрителей группы (refcount без самого энкодера).
    pub viewers: usize,
    /// `P1`: число занятых ячеек композита комнаты (видимые окна) на момент
    /// снимка. Заполняет движок в `video_farm_stats`; ферма только хранит.
    pub visible_windows: usize,
    /// RTP-пакетов отправлено зрителям группы (`P4a`).
    pub viewer_packets: u64,
    /// Байт payload отправлено зрителям группы (`P4a`).
    pub viewer_bytes: u64,
    /// Кадров потеряно зрителями (Lagged/IDR-гейт) (`P4a`).
    pub viewer_dropped: u64,
}

/// Группа: энкодер + подписчики.
struct Group {
    key: EncoderKey,
    /// Зрители + 1 (сам энкодер) — как в спеке §B.2.
    refcount: AtomicUsize,
    /// Последняя публикация (мкс от старта группы): дедуп одинаковых кадров
    /// от N сессий, публикующих один композит.
    last_publish_us: AtomicU64,
    /// Счётчик запросов keyframe (PLI/FIR зрителей).
    keyframe_requests: AtomicU64,
    /// Всего запросов с момента создания группы.
    keyframe_requests_total: AtomicU64,
    frames: AtomicU64,
    keyframes: AtomicU64,
    dropped_frames: AtomicU64,
    restarts: AtomicU64,
    /// `P4a`: агрегат per-viewer RTP-статистики группы (см. [`ViewerRtp`]).
    rtp: Arc<ViewerRtp>,
    /// Последние SPS/PPS: новому зрителю нужны до первого IDR (спека §B.6.4).
    parameter_sets: Mutex<Vec<Arc<[u8]>>>,
    /// Канал кадров в актор (ёмкость 1: медленный энкодер не копит очередь).
    input: mpsc::Sender<Arc<Frame>>,
    tx: broadcast::Sender<Arc<AccessUnit>>,
    cancel: Arc<AtomicBool>,
    notify: Arc<tokio::sync::Notify>,
    /// PID child'а (= PGID: спавн с `process_group(0)`). Нужен, чтобы убить
    /// группу процессов из retire/Drop даже если актор не успел это сделать.
    pid: AtomicU32,
    started: Instant,
    join: Mutex<Option<JoinHandle<()>>>,
}

impl Group {
    fn refcount(&self) -> usize {
        self.refcount.load(Ordering::Relaxed)
    }
}

/// Non-owning подписка рекордера на **уже существующую** группу фермы.
///
/// Не увеличивает `refcount`, не создаёт группу и не спавнит FFmpeg: если
/// группа ретируется, `rx.recv()` вернёт `Closed`. Это семантика «нет
/// зрителя — нет записи» (мастер-план: tap = subscribe, а не hidden viewer).
pub struct FarmTap {
    /// Ключ группы, на которую подписались (room + canvas_version + spec).
    pub key: EncoderKey,
    /// Поток access units группы.
    pub rx: broadcast::Receiver<Arc<AccessUnit>>,
    /// SPS/PPS snapshot на момент подписки (может быть пустым до первого IDR).
    pub parameter_sets: Vec<Arc<[u8]>>,
}

/// Жива ли группа: не отменена и её актор ещё не завершился.
fn group_is_live(group: &Group) -> bool {
    !group.cancel.load(Ordering::Relaxed)
        && !group.join.lock().unwrap().as_ref().is_some_and(|actor| actor.is_finished())
}

/// Ферма энкодеров (одна на [`crate::engine::McuEngine`]).
pub struct EncoderFarm {
    inner: Arc<FarmInner>,
}

struct FarmInner {
    groups: Mutex<BTreeMap<EncoderKey, Arc<Group>>>,
    versions: Mutex<BTreeMap<RoomId, u64>>,
    /// Сериализует медленный путь `acquire` (проверка → spawn child → вставка
    /// в мапу): иначе два-три render-таска сессий одновременно спавнят FFmpeg
    /// на один ключ; второй `insert` затирал первый, и осиротевший child с
    /// актором жил вечно (утечка «4 → 8 → 12» на живом прогоне).
    creation: tokio::sync::Mutex<()>,
    max_groups: usize,
    spawner: Spawner,
}

/// Фабрика child-процесса: подменяется в тестах (без запуска FFmpeg).
type Spawner = Arc<dyn Fn(&EncoderSpec, (u32, u32)) -> io::Result<Child> + Send + Sync>;

impl EncoderFarm {
    /// Создаёт ферму с лимитом групп из `VKS_VIDEO_FARM_MAX_GROUPS`
    /// (по умолчанию 128; каждая группа — один FFmpeg-процесс).
    pub fn new() -> Self {
        Self::with_spawner(default_spawner(), group_limit_from_env())
    }

    fn with_spawner(spawner: Spawner, max_groups: usize) -> Self {
        Self {
            inner: Arc::new(FarmInner {
                groups: Mutex::new(BTreeMap::new()),
                versions: Mutex::new(BTreeMap::new()),
                creation: tokio::sync::Mutex::new(()),
                max_groups: max_groups.max(1),
                spawner,
            }),
        }
    }

    /// Текущая версия холста комнаты (инкрементится [`Self::invalidate_room`]).
    pub fn canvas_version(&self, room: RoomId) -> u64 {
        self.inner.versions.lock().unwrap().get(&room).copied().unwrap_or(0)
    }

    /// Подписка рекордера на существующую группу: room + codec + текущая
    /// версия холста, группа жива, `viewers > 0`.
    ///
    /// Никакого `spawner`/`insert`/`refcount`: это не зритель. При нескольких
    /// подходящих группах выбирается группа с максимумом зрителей; при
    /// равенстве — первая в порядке `BTreeMap` (детерминированно).
    /// `None` — запись невозможна без скрытой группы (admin отдаёт 409).
    pub fn subscribe(&self, room: RoomId, codec: FarmCodec) -> Option<FarmTap> {
        let canvas_version = self.canvas_version(room);
        let groups = self.inner.groups.lock().unwrap();
        let mut selected: Option<&Arc<Group>> = None;
        for group in groups.values() {
            if group.key.room != room
                || group.key.spec.codec != codec
                || group.key.canvas_version != canvas_version
                || !group_is_live(group)
                || group.refcount() <= 1
            {
                continue;
            }
            if selected.is_none_or(|current| group.refcount() > current.refcount()) {
                selected = Some(group);
            }
        }
        let group = selected?;
        let key = group.key.clone();
        let rx = group.tx.subscribe();
        let parameter_sets = group.parameter_sets.lock().unwrap().clone();
        drop(groups);
        Some(FarmTap { key, rx, parameter_sets })
    }

    /// Число групп комнаты, к которым можно подписаться (room + codec +
    /// текущий canvas + live + `viewers > 0`). Для 409-гейта записи.
    pub fn active_group_count(&self, room: RoomId, codec: FarmCodec) -> usize {
        let canvas_version = self.canvas_version(room);
        let groups = self.inner.groups.lock().unwrap();
        groups
            .values()
            .filter(|group| {
                group.key.room == room
                    && group.key.spec.codec == codec
                    && group.key.canvas_version == canvas_version
                    && group_is_live(group)
                    && group.refcount() > 1
            })
            .count()
    }

    /// Есть ли активная H.264-группа со зрителями (recording 409-гейт).
    pub fn has_active_h264_group(&self, room: RoomId) -> bool {
        self.active_group_count(room, FarmCodec::H264) > 0
    }

    /// Идемпотентно берёт группу под ключ; `input` — размер кадра, который
    /// будут публиковать сессии (холст комнаты).
    ///
    /// Возвращает [`GroupViewer`] с broadcast-приёмником; `Drop` зрителя
    /// уменьшает refcount и убивает child при нуле.
    pub async fn acquire(&self, key: EncoderKey, input: (u32, u32)) -> Result<GroupViewer, FarmError> {
        // Ключ старой версии холста после `invalidate_room` закрыт: не плодим
        // child, который не получит ни одного кадра (зритель переподключится
        // с актуальной версией).
        if key.canvas_version != self.canvas_version(key.room) {
            return Err(FarmError::Closed);
        }
        {
            let mut groups = self.inner.groups.lock().unwrap();
            if let Some(group) = groups.get(&key) {
                let dead = group.cancel.load(Ordering::Relaxed)
                    || group.join.lock().unwrap().as_ref().is_some_and(|actor| actor.is_finished());
                if dead {
                    let stale = groups.remove(&key);
                    drop(stale);
                    return Err(FarmError::Closed);
                }
                group.refcount.fetch_add(1, Ordering::Relaxed);
                let viewer = self.viewer(key.clone(), group);
                drop(groups);
                return Ok(viewer);
            }
            if groups.len() >= self.inner.max_groups {
                return Err(FarmError::Overloaded { limit: self.inner.max_groups });
            }
        }
        // Медленный путь только один на ключ: иначе гонка spawn/insert
        // плодит дубли группы и сиротских FFmpeg (утечка на живом прогоне).
        let _creating = self.inner.creation.lock().await;
        {
            let groups = self.inner.groups.lock().unwrap();
            if let Some(group) = groups.get(&key) {
                // Пока ждали мьютекс, группу могли создать — переиспользуем.
                group.refcount.fetch_add(1, Ordering::Relaxed);
                let viewer = self.viewer(key.clone(), group);
                drop(groups);
                return Ok(viewer);
            }
            if groups.len() >= self.inner.max_groups {
                return Err(FarmError::Overloaded { limit: self.inner.max_groups });
            }
        }
        let (input_tx, input_rx) = mpsc::channel::<Arc<Frame>>(1);
        let (tx, _) = broadcast::channel::<Arc<AccessUnit>>(4);
        let cancel = Arc::new(AtomicBool::new(false));
        let notify = Arc::new(tokio::sync::Notify::new());
        let spec = key.spec.clone();
        let input_dims = (input.0.max(2) & !1, input.1.max(2) & !1);
        let child = match (self.inner.spawner)(&spec, input_dims) {
            Ok(child) => child,
            Err(error) => return Err(FarmError::Spawn(error.to_string())),
        };
        let child_pid = child.id().unwrap_or(0);
        let group = Arc::new(Group {
            key: key.clone(),
            refcount: AtomicUsize::new(2), // зритель + энкодер
            last_publish_us: AtomicU64::new(0),
            keyframe_requests: AtomicU64::new(0),
            keyframe_requests_total: AtomicU64::new(0),
            frames: AtomicU64::new(0),
            keyframes: AtomicU64::new(0),
            dropped_frames: AtomicU64::new(0),
            restarts: AtomicU64::new(0),
            rtp: Arc::new(ViewerRtp::default()),
            parameter_sets: Mutex::new(Vec::new()),
            input: input_tx,
            tx,
            cancel: cancel.clone(),
            notify: notify.clone(),
            pid: AtomicU32::new(child_pid),
            started: Instant::now(),
            join: Mutex::new(None),
        });
        // Порядок важен: сначала мапа (actor-watchdog считает `Arc::strong_count`,
        // и «1» означает «группы нет ни в мапе, ни у зрителей»), потом актор.
        self.inner.groups.lock().unwrap().insert(key.clone(), group.clone());
        let actor = tokio::spawn(run_group(
            group.clone(),
            child,
            input_rx,
            spec.clone(),
            cancel,
            notify,
            Arc::downgrade(&self.inner),
        ));
        *group.join.lock().unwrap() = Some(actor);
        tracing::info!(
            room = %key.room,
            width = input_dims.0,
            height = input_dims.1,
            output = %format!("{}x{}", spec.width, spec.height),
            fps = spec.fps,
            groups = self.inner.groups.lock().unwrap().len(),
            "encoder group started"
        );
        Ok(self.viewer(key, &group))
    }

    fn viewer(&self, key: EncoderKey, group: &Arc<Group>) -> GroupViewer {
        let parameter_sets = group.parameter_sets.lock().unwrap().clone();
        GroupViewer {
            key,
            rx: group.tx.subscribe(),
            parameter_sets,
            farm: self.inner.clone(),
            group: group.clone(),
            released: false,
        }
    }

    /// Публикует общий кадр комнаты всем её группам (нулевые копии: `Arc`).
    ///
    /// Дедуп по времени кадра: 20 сессий публикуют один композит, кодируется
    /// один. Возвращает число групп, принявших кадр.
    pub fn publish(&self, room: RoomId, frame: &Arc<Frame>) -> usize {
        let version = self.canvas_version(room);
        let groups: Vec<Arc<Group>> = {
            let groups = self.inner.groups.lock().unwrap();
            groups
                .values()
                .filter(|group| group.key.room == room && group.key.canvas_version == version)
                .cloned()
                .collect()
        };
        let mut accepted = 0;
        for group in groups {
            if group.cancel.load(Ordering::Relaxed) {
                continue;
            }
            let now_us = group.started.elapsed().as_micros() as u64;
            let min_gap_us = (group.key.spec.frame_interval().as_micros() as u64) * 3 / 4;
            let previous = group.last_publish_us.load(Ordering::Relaxed);
            if previous != 0 && now_us.saturating_sub(previous) < min_gap_us {
                continue;
            }
            group.last_publish_us.store(now_us, Ordering::Relaxed);
            match group.input.try_send(frame.clone()) {
                Ok(()) => accepted += 1,
                Err(_) => {
                    group.dropped_frames.fetch_add(1, Ordering::Relaxed);
                }
            }
        }
        accepted
    }

    /// Просит IDR у всех групп комнаты (PLI/FIR любого зрителя).
    ///
    /// v1: запрос учитывается и виден в метриках, но IDR не форсируется —
    /// `keyint = fps` даёт ≤1 с; рестарт child'а рвёт поток всем зрителям
    /// (спека §B.6). Форс-IDR — P4b вместе с control-каналом энкодера.
    pub fn request_keyframe(&self, room: RoomId) {
        let groups: Vec<Arc<Group>> = {
            let groups = self.inner.groups.lock().unwrap();
            groups.values().filter(|group| group.key.room == room).cloned().collect()
        };
        for group in groups {
            group.keyframe_requests.fetch_add(1, Ordering::Relaxed);
            group.keyframe_requests_total.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Смена холста/раскладки: новые группы — с новой версией, старые
    /// закрываются (их зрители получат `Closed` и переподключатся путём вызова).
    pub fn invalidate_room(&self, room: RoomId) {
        let version = {
            let mut versions = self.inner.versions.lock().unwrap();
            let entry = versions.entry(room).or_insert(0);
            *entry = entry.saturating_add(1);
            *entry
        };
        let stale: Vec<Arc<Group>> = {
            let groups = self.inner.groups.lock().unwrap();
            groups
                .values()
                .filter(|group| group.key.room == room && group.key.canvas_version != version)
                .cloned()
                .collect()
        };
        for group in stale {
            tracing::info!(room = %room, version, "encoder group invalidated by canvas change");
            // Не оставляем child'а жить: смена холста — это смерть группы.
            self.retire(&group.key);
        }
    }

    /// Диагностика всех групп (`/metrics`, `/api/v3`).
    pub fn snapshot(&self) -> Vec<GroupStat> {
        let groups = self.inner.groups.lock().unwrap();
        groups
            .values()
            .map(|group| GroupStat {
                key: group.key.clone(),
                refcount: group.refcount(),
                frames: group.frames.load(Ordering::Relaxed),
                keyframes: group.keyframes.load(Ordering::Relaxed),
                dropped_frames: group.dropped_frames.load(Ordering::Relaxed),
                restarts: group.restarts.load(Ordering::Relaxed),
                keyframe_requests: group.keyframe_requests_total.load(Ordering::Relaxed),
                viewers: group.refcount().saturating_sub(1),
                // Заполняет движок (`video_farm_stats`): ферма не знает раскладку.
                visible_windows: 0,
                viewer_packets: group.rtp.packets(),
                viewer_bytes: group.rtp.bytes(),
                viewer_dropped: group.rtp.dropped_frames(),
            })
            .collect()
    }

    /// Число живых групп.
    pub fn group_count(&self) -> usize {
        self.inner.groups.lock().unwrap().len()
    }

    /// Убирает группу из карты (актор уже остановлен) и ждёт его завершения.
    fn retire(&self, key: &EncoderKey) {
        let group = self.inner.groups.lock().unwrap().remove(key);
        let Some(group) = group else { return };
        group.cancel.store(true, Ordering::Relaxed);
        group.notify.notify_waiters();
        let room = group.key.room;
        let pid = group.pid.swap(0, Ordering::AcqRel);
        // `retire` = группа уничтожается: child не должен пережить её ни на
        // миллисекунду. Бьём группу процессов сразу (актор потом повторит).
        kill_process_group(pid);
        let actor = group.join.lock().unwrap().take();
        if let Some(actor) = actor {
            tokio::spawn(async move {
                match tokio::time::timeout(Duration::from_secs(5), actor).await {
                    Err(_) => {
                        // Актор завис (напр. потерянный wakeup) — добиваем
                        // группу процессов жёстко, чтобы не копить сирот.
                        tracing::warn!(%room, "encoder group actor did not stop within 5 s; killing process group");
                        kill_process_group(pid);
                    }
                    Ok(_) => {
                        kill_process_group(pid);
                        tracing::debug!(%room, "encoder group retired");
                    }
                }
            });
        } else {
            kill_process_group(pid);
        }
    }
}

impl Default for EncoderFarm {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for EncoderFarm {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("EncoderFarm")
            .field("groups", &self.group_count())
            .field("max_groups", &self.inner.max_groups)
            .finish()
    }
}

/// Зритель группы: broadcast-приёмник AU + авто-release при `Drop`.
pub struct GroupViewer {
    /// Ключ группы (диагностика/инвалидация).
    pub key: EncoderKey,
    /// Поток access units группы (собственный RTP-обвязчик — у зрителя).
    pub rx: broadcast::Receiver<Arc<AccessUnit>>,
    /// SPS/PPS на момент подключения: новому зрителю нужны до первого IDR.
    pub parameter_sets: Vec<Arc<[u8]>>,
    farm: Arc<FarmInner>,
    group: Arc<Group>,
    released: bool,
}

impl GroupViewer {
    /// `P4a`: учитывает отправку AU этому зрителю (пакеты/байты RTP).
    pub fn note_rtp(&self, packets: u64, bytes: u64) {
        self.group.rtp.note_sent(packets, bytes);
    }

    /// `P4a`: учитывает кадры, потерянные этим зрителем (Lagged/IDR-гейт).
    pub fn note_dropped(&self, frames: u64) {
        self.group.rtp.note_dropped(frames);
    }

    /// `P4a`: снимок RTP-статистики зрителей группы (packets, bytes, dropped).
    #[must_use]
    pub fn rtp_snapshot(&self) -> (u64, u64, u64) {
        self.group.rtp.snapshot()
    }

    /// Ранний release (до истечения сессии): идемпотентно.
    pub fn release(mut self) {
        self.release_inner();
    }

    fn release_inner(&mut self) {
        if self.released {
            return;
        }
        self.released = true;
        let previous = self.group.refcount.fetch_sub(1, Ordering::Relaxed);
        if previous <= 2 {
            // Остался только энкодер: останавливаем группу.
            let farm = EncoderFarm { inner: self.farm.clone() };
            farm.retire(&self.key);
        }
    }

    /// Сколько зрителей (включая энкодер) осталось в группе.
    pub fn refcount(&self) -> usize {
        self.group.refcount()
    }
}

impl std::fmt::Debug for GroupViewer {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("GroupViewer")
            .field("room", &self.key.room)
            .field("refcount", &self.refcount())
            .field("parameter_sets", &self.parameter_sets.len())
            .finish()
    }
}

impl Drop for GroupViewer {
    fn drop(&mut self) {
        self.release_inner();
    }
}

/// Актор группы: две независимые половины.
///
/// **Писатель** (этот таск): кадры из [`mpsc`] → stdin child'а.
/// **Читатель** (отдельный таск): stdout → Annex-B → access units → broadcast.
///
#[cfg(target_os = "linux")]
pub fn tune_pipe_capacity<F: std::os::unix::io::AsRawFd>(file: &F, is_raw_video: bool) {
    let fd = file.as_raw_fd();
    let ladder: &[i32] = if is_raw_video {
        &[4 * 1024 * 1024, 2 * 1024 * 1024, 1024 * 1024, 512 * 1024, 256 * 1024, 64 * 1024]
    } else {
        &[256 * 1024, 128 * 1024, 64 * 1024]
    };
    for &cap in ladder {
        let ret = unsafe { libc::fcntl(fd, libc::F_SETPIPE_SZ, cap) };
        if ret > 0 {
            break;
        }
    }
}
#[cfg(not(target_os = "linux"))]
pub fn tune_pipe_capacity<F>(_file: &F, _is_raw_video: bool) {}

/// Разделение принципиально: если читать stdout в том же цикле, что писать
/// stdin, возникает клинч — границей AU служит AUD **следующего** кадра, а его
/// нельзя закодировать, пока писатель не отдал следующий кадр, но писатель
/// заблокирован на чтении. На живом прогоне это давало 1 AU (IDR) и тишину:
/// энкодер получал единицы кадров, зритель — один стартовый кадр.
async fn run_group(
    group: Arc<Group>,
    mut child: Child,
    mut input: mpsc::Receiver<Arc<Frame>>,
    spec: EncoderSpec,
    cancel: Arc<AtomicBool>,
    notify: Arc<tokio::sync::Notify>,
    farm: Weak<FarmInner>,
) {
    let mut stdin = child.stdin.take();
    let stdout = child.stdout.take();
    if let Some(ref p) = stdin {
        tune_pipe_capacity(p, true);
    }
    if let Some(ref p) = stdout {
        tune_pipe_capacity(p, false);
    }
    let reader = tokio::spawn(read_access_units(group.clone(), stdout, spec.clone(), cancel.clone()));
    // Кадров записано / AU отправлено — для диагностики «энкодер не голодает?».
    let mut frames_written: u64 = 0;
    let io_timeout = Duration::from_secs(2);
    // Периодический тик: `notify_waiters()` не будит актора, если тот в этот
    // момент не ждал (потерянный wakeup) — без тика актор висел на `input.recv()`
    // вечно, а child (ffmpeg) оставался жить после ретира группы.
    let mut cancel_tick = tokio::time::interval(Duration::from_millis(20));
    cancel_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    while !cancel.load(Ordering::Relaxed) {
        let frame = tokio::select! {
            frame = input.recv() => match frame {
                Some(frame) => frame,
                None => break,
            },
            () = notify.notified() => break,
            _ = cancel_tick.tick() => {
                // Страховка от сирот: если группы больше нет в мапе фермы
                // (гонка insert/retire, будущий баг), актор обязан выйти и
                // убить child — иначе ffmpeg живёт вечно, ждёт stdin и не
                // reap'ится. Читатель держит свой Arc, поэтому `strong_count`
                // здесь не годится — сверяемся с мапой по указателю.
                let registered = match farm.upgrade() {
                    Some(farm) => {
                        let groups = farm.groups.lock().unwrap();
                        groups.get(&group.key).is_some_and(|known| Arc::ptr_eq(known, &group))
                    }
                    // Сама ферма уничтожена — тоже сирота.
                    None => false,
                };
                if !registered {
                    tracing::warn!(room = %group.key.room, "encoder group orphaned; actor self-terminating");
                    break;
                }
                continue;
            }
        };
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        let Some(pipe) = stdin.as_mut() else { break };
        if tokio::time::timeout(io_timeout, pipe.write_all(&frame.data)).await.map_or(true, |result| result.is_err()) {
            // Энкодер не принимает кадры: группа закрывается, зрители получают
            // `Closed` (gateway логирует и завершает видео-мост).
            if !cancel.load(Ordering::Relaxed) && !group.cancel.load(Ordering::Relaxed) {
                tracing::warn!(room = %group.key.room, "encoder group failed: stdin closed");
            }
            group.cancel.store(true, Ordering::Relaxed);
            break;
        }
        frames_written += 1;
        let requested = group.keyframe_requests.swap(0, Ordering::Relaxed);
        if requested > 0 {
            tracing::debug!(
                room = %group.key.room,
                requested,
                keyint = spec.keyint,
                "group keyframe request consumed (IDR follows within keyint; force-IDR is P4b)"
            );
        }
        if frames_written.is_multiple_of(30) {
            let encoded = group.frames.load(Ordering::Relaxed);
            if frames_written.saturating_sub(encoded) > frames_written / 2 {
                tracing::warn!(
                    room = %group.key.room,
                    frames_written,
                    encoded,
                    dropped = group.dropped_frames.load(Ordering::Relaxed),
                    "encoder group is starving: AU output lags input"
                );
            } else {
                tracing::debug!(room = %group.key.room, frames_written, encoded, "encoder group progress");
            }
        }
    }
    cancel.store(true, Ordering::Relaxed);
    notify.notify_waiters();
    reader.abort();
    let _ = reader.await;
    // Останавливаем child и всю группу процессов (ADR-0028) и прибираем его.
    kill_child(&mut child).await;
    tracing::debug!(room = %group.key.room, "encoder group actor stopped");
}

/// Читатель группы: пока ребёнок жив, непрерывно читает stdout и собирает AU.
///
/// Работает параллельно писателю — именно это даёт полный fps: AUD каждого
/// следующего кадра приходит сразу после его кодирования, без ожидания
/// писателя. Последний AU (без следующего AUD) флашится по EOF/остановке.
async fn read_access_units(
    group: Arc<Group>,
    stdout: Option<tokio::process::ChildStdout>,
    spec: EncoderSpec,
    cancel: Arc<AtomicBool>,
) {
    let Some(mut stdout) = stdout else { return };
    let started = Instant::now();
    let mut splitter = AnnexB::default();
    let mut pending: Vec<u8> = Vec::new();
    let mut pending_sets: Vec<Arc<[u8]>> = Vec::new();
    let mut buffer = [0u8; 65536];
    loop {
        let count = tokio::select! {
            result = stdout.read(&mut buffer) => match result {
                Ok(0) | Err(_) => break,
                Ok(count) => count,
            },
            () = wait_cancelled(&cancel) => break,
        };
        for nal in splitter.push(&buffer[..count]) {
            if is_aud(&nal, spec.codec) && !pending.is_empty() {
                emit_access_unit(&group, &spec, &mut pending, &mut pending_sets, started);
            }
            if is_parameter_set(&nal, spec.codec) {
                let set: Arc<[u8]> = Arc::from(nal.as_slice());
                if !pending_sets.iter().any(|existing| existing.as_ref() == set.as_ref()) {
                    pending_sets.push(set);
                }
            }
            pending.extend_from_slice(&nal);
        }
    }
    // Хвост: энкодер остановлен, но последний кадр не получил AUD-терминатор.
    if !pending.is_empty() {
        emit_access_unit(&group, &spec, &mut pending, &mut pending_sets, started);
    }
}

/// Ждёт отмены группы (для `select!` в читателе).
async fn wait_cancelled(cancel: &Arc<AtomicBool>) {
    while !cancel.load(Ordering::Relaxed) {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// Собирает AU из накопленных NAL, обновляет кэш SPS/PPS и рассылает зрителям.
fn emit_access_unit(
    group: &Group,
    spec: &EncoderSpec,
    pending: &mut Vec<u8>,
    pending_sets: &mut Vec<Arc<[u8]>>,
    started: Instant,
) {
    let data: Arc<[u8]> = Arc::from(std::mem::take(pending).as_slice());
    let is_idr = split_nals(&data).iter().any(|nal| spec.codec.is_idr_nal(nal));
    let parameter_sets = std::mem::take(pending_sets);
    if !parameter_sets.is_empty() {
        *group.parameter_sets.lock().unwrap() = parameter_sets.clone();
    }
    let au = Arc::new(AccessUnit { data, is_idr, pts_us: started.elapsed().as_micros() as u64, parameter_sets });
    group.frames.fetch_add(1, Ordering::Relaxed);
    if is_idr {
        group.keyframes.fetch_add(1, Ordering::Relaxed);
    }
    // `send` ошибается, только когда зрителей нет (кадры всё равно кодируются,
    // пока группа жива) — это не потеря, поэтому ошибку не считаем.
    let _ = group.tx.send(au);
}

/// AUD (access-unit delimiter): H.264 — NAL type 9, H.265 — NAL type 35.
fn is_aud(nal: &[u8], codec: FarmCodec) -> bool {
    let Some((_, start)) = find_start_code(nal, 0) else { return false };
    let Some(&header) = nal.get(start) else { return false };
    match codec {
        FarmCodec::H264 => header & 0x1f == 9,
        FarmCodec::H265 => (header >> 1) & 0x3f == 35,
    }
}

fn is_parameter_set(nal: &[u8], codec: FarmCodec) -> bool {
    codec.is_parameter_set(nal)
}

/// Фабрика по умолчанию: `tokio::process` + FFmpeg с теми же аргументами,
/// что были в per-session пути (level/profile/keyint/maxrate/bufsize).
fn default_spawner() -> Spawner {
    Arc::new(|spec: &EncoderSpec, input: (u32, u32)| spawn_ffmpeg(spec, input))
}

fn spawn_ffmpeg(spec: &EncoderSpec, input: (u32, u32)) -> io::Result<Child> {
    let mut command = vks_common::process::safe_ffmpeg_command();
    command.args(["-hide_banner", "-loglevel", "error", "-nostdin", "-threads", "2", "-filter_threads", "2"]);
    command.args([
        "-f",
        "rawvideo",
        "-pixel_format",
        "yuv420p",
        "-video_size",
        &format!("{}x{}", input.0, input.1),
        "-framerate",
        &spec.fps.to_string(),
        "-i",
        "pipe:0",
        "-an",
    ]);
    if (input.0, input.1) != (spec.width, spec.height) {
        // Egress-resize уезжает в энкодер: в parent его больше нет (спека §B.4 п.3).
        command.args(["-vf", &format!("scale={}:{}", spec.width, spec.height)]);
    }
    command.args([
        "-c:v",
        spec.codec.encoder(),
        "-threads",
        "2",
        "-preset",
        "ultrafast",
        "-tune",
        "zerolatency",
        "-pix_fmt",
        "yuv420p",
    ]);
    if spec.codec == FarmCodec::H264 {
        command.args(["-profile:v", spec.profile.as_str(), "-level:v", spec.level.as_str()]);
    }
    let keyint_min = (spec.fps / 2).max(1);
    command.args([
        "-b:v",
        &format!("{}k", spec.bitrate_kbps),
        "-maxrate",
        &format!("{}k", spec.maxrate_kbps),
        "-bufsize",
        &format!("{}k", spec.bufsize_kbps),
        "-g",
        &spec.keyint.to_string(),
        "-keyint_min",
        &keyint_min.to_string(),
        spec.codec.params_flag(),
        spec.encoder_params.as_str(),
    ]);
    // `-flush_packets 1`: без него FFmpeg копит выход в буфере и группа
    // отдаёт первый AU с задержкой в несколько кадров.
    command.args(["-flush_packets", "1", "-f", spec.codec.muxer(), "pipe:1"]);
    command.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
    command.spawn()
}

/// Убивает группу процессов по PGID (child спавнится с `process_group(0)`,
/// поэтому его pid == pgid). ESRCH/EPERM игнорируются: группа могла умереть.
fn kill_process_group(pid: u32) {
    vks_common::process::kill_process_group(pid);
}

/// Убивает child и всю его группу процессов (ADR-0028), затем прибирает
/// (`wait`), чтобы не оставлять зомби/сирот.
async fn kill_child(child: &mut Child) {
    if let Some(pid) = child.id() {
        kill_process_group(pid);
    }
    let _ = child.start_kill();
    // `wait` обязателен: без него child остаётся зомби (а при потере wakeup
    // актор жил, и ffmpeg висел с открытыми пайпами).
    let _ = tokio::time::timeout(Duration::from_secs(2), child.wait()).await;
}

/// Backstop: ферма уничтожается — не оставляем ни одного child'а.
impl Drop for FarmInner {
    fn drop(&mut self) {
        let groups: Vec<Arc<Group>> = self.groups.lock().unwrap().values().cloned().collect();
        for group in groups {
            group.cancel.store(true, Ordering::Relaxed);
            group.notify.notify_waiters();
            kill_process_group(group.pid.swap(0, Ordering::AcqRel));
        }
    }
}

/// Границы Annex-B: режет поток на NAL с start code.
#[derive(Default)]
struct AnnexB {
    buffer: Vec<u8>,
}

impl AnnexB {
    /// Отдаёт только завершённые NAL: последний NAL потока ждёт следующего
    /// start code (границей AU служит AUD, он приходит первым).
    fn push(&mut self, bytes: &[u8]) -> Vec<Vec<u8>> {
        self.buffer.extend_from_slice(bytes);
        let mut units = Vec::new();
        while let Some((first, body)) = find_start_code(&self.buffer, 0) {
            if first > 0 {
                // Мусор до первого start code игнорируем.
                self.buffer.drain(..first);
                continue;
            }
            let Some((next, _)) = find_start_code(&self.buffer, body) else { break };
            units.push(self.buffer.drain(..next).collect());
        }
        units
    }
}

/// Режет готовый буфер на NAL (последний — до конца буфера).
fn split_nals(bytes: &[u8]) -> Vec<Vec<u8>> {
    let mut units = Vec::new();
    let mut cursor = 0;
    while let Some((start, body)) = find_start_code(bytes, cursor) {
        match find_start_code(bytes, body) {
            Some((next, _)) => {
                units.push(bytes[start..next].to_vec());
                cursor = next;
            }
            None => {
                units.push(bytes[start..].to_vec());
                break;
            }
        }
    }
    units
}

/// Находит start code (`00 00 01` / `00 00 00 01`).
fn find_start_code(bytes: &[u8], from: usize) -> Option<(usize, usize)> {
    let mut index = from;
    while index + 3 <= bytes.len() {
        if bytes[index] == 0 && bytes[index + 1] == 0 {
            if bytes[index + 2] == 1 {
                return Some((index, index + 3));
            }
            if index + 4 <= bytes.len() && bytes[index + 2] == 0 && bytes[index + 3] == 1 {
                return Some((index, index + 4));
            }
        }
        index += 1;
    }
    None
}

fn group_limit_from_env() -> usize {
    std::env::var("VKS_VIDEO_FARM_MAX_GROUPS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(128)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn room() -> RoomId {
        RoomId::new()
    }

    fn spec(fps: u32) -> EncoderSpec {
        EncoderSpec::canonical(FarmCodec::H264, 640, 360, fps, 800, "baseline", "3.1")
    }

    /// Fake-фабрика без FFmpeg: актор пишет кадры в stdin `cat`, а `cat`
    /// возвращает их в stdout. Annex-B в таких байтах не находится, поэтому
    /// AU не эмитятся — тесты проверяют учёт/refcount/дедуп/инвалидацию
    /// (сам конвейер кодирования проверяется интеграционными тестами
    /// `vks-server` с реальным FFmpeg).
    fn fake_spawner() -> Spawner {
        Arc::new(|_spec: &EncoderSpec, _input: (u32, u32)| {
            let mut command = vks_common::process::safe_command("cat");
            command.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
            command.spawn()
        })
    }

    fn fake_frame(value: u8) -> Arc<Frame> {
        Arc::new(Frame::new(64, 64, vec![value; 64 * 64]))
    }

    /// Ферма с фейковым энкодером: подменяем spawner на `cat`, а AU
    /// генерируем прямо в актор — это проверяет только учёт/refcount.
    fn farm_with_test_spawner(max_groups: usize) -> EncoderFarm {
        EncoderFarm::with_spawner(fake_spawner(), max_groups)
    }

    /// Реальные-ffmpeg тесты считают детей процесса: их нельзя гонять
    /// параллельно между собой (иначе чужой энкодер попадёт в счёт).
    static FFMPEG_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    fn ffmpeg_available() -> bool {
        std::process::Command::new("ffmpeg")
            .arg("-version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok()
    }

    /// Дети текущего процесса, чей cmdline содержит подстроку (по `/proc`).
    fn children_with(cmdline_needle: &str) -> usize {
        let me = std::process::id();
        let Ok(entries) = std::fs::read_dir("/proc") else { return 0 };
        entries
            .flatten()
            .filter(|entry| entry.file_name().to_string_lossy().chars().all(|ch| ch.is_ascii_digit()))
            .filter(|entry| {
                let path = entry.path();
                let stat = std::fs::read_to_string(path.join("stat")).unwrap_or_default();
                let Some(close) = stat.rfind(')') else { return false };
                let fields: Vec<&str> = stat[close + 1..].split_whitespace().collect();
                let ppid: u32 = fields.get(1).and_then(|value| value.parse().ok()).unwrap_or(0);
                if ppid != me {
                    return false;
                }
                std::fs::read_to_string(path.join("cmdline"))
                    .map(|cmdline| cmdline.contains(cmdline_needle))
                    .unwrap_or(false)
            })
            .count()
    }

    fn ffmpeg_children() -> usize {
        children_with("ffmpeg")
    }

    /// Дети-зомби текущего процесса (`stat` = `Z`): должны отсутствовать —
    /// иначе `pgrep -c ffmpeg` продолжает их считать после утечки.
    fn zombie_children() -> usize {
        let me = std::process::id();
        let Ok(entries) = std::fs::read_dir("/proc") else { return 0 };
        entries
            .flatten()
            .filter(|entry| entry.file_name().to_string_lossy().chars().all(|ch| ch.is_ascii_digit()))
            .filter(|entry| {
                let stat = std::fs::read_to_string(entry.path().join("stat")).unwrap_or_default();
                let Some(close) = stat.rfind(')') else { return false };
                let fields: Vec<&str> = stat[close + 1..].split_whitespace().collect();
                let state = fields.first().copied().unwrap_or("");
                let ppid: u32 = fields.get(1).and_then(|value| value.parse().ok()).unwrap_or(0);
                ppid == me && state == "Z"
            })
            .count()
    }

    async fn wait_until(mut condition: impl FnMut() -> bool, message: &str) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while !condition() {
            assert!(tokio::time::Instant::now() < deadline, "timeout: {message}");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    #[test]
    fn annexb_splits_nals_and_keeps_partial() {
        let mut splitter = AnnexB::default();
        let first = splitter.push(&[0, 0, 1, 0x09, 0xf0, 0, 0, 1, 0x65]);
        assert_eq!(first.len(), 1, "первый NAL без завершающего start code не отдаётся");
        let second = splitter.push(&[7, 0, 0, 1, 0x41]);
        assert_eq!(second.len(), 1, "второй NAL отдаётся по границе");
        assert_eq!(second[0], vec![0, 0, 1, 0x65, 7]);
    }

    #[test]
    fn access_unit_delimiters_are_codec_aware() {
        assert!(is_aud(&[0, 0, 1, 0x09, 0xf0], FarmCodec::H264));
        assert!(!is_aud(&[0, 0, 1, 0x65, 1], FarmCodec::H264));
        // H.265 AUD: nal_unit_type 35 → header 0x46.
        assert!(is_aud(&[0, 0, 1, 0x46, 0x01, 0x50], FarmCodec::H265));
        assert!(!is_aud(&[0, 0, 1, 0x09, 0xf0], FarmCodec::H265), "H.264 AUD не H.265 AUD");
    }

    #[test]
    fn nal_classification_h264_and_h265() {
        assert!(FarmCodec::H264.is_idr_nal(&[0, 0, 1, 0x65]));
        assert!(!FarmCodec::H264.is_idr_nal(&[0, 0, 1, 0x41]));
        assert!(FarmCodec::H264.is_parameter_set(&[0, 0, 1, 0x67]));
        assert!(FarmCodec::H264.is_parameter_set(&[0, 0, 1, 0x68]));
        assert!(!FarmCodec::H264.is_parameter_set(&[0, 0, 1, 0x65]));
        // H.265: type = (header >> 1) & 0x3f.
        assert!(FarmCodec::H265.is_idr_nal(&[0, 0, 1, 19 << 1]));
        assert!(FarmCodec::H265.is_parameter_set(&[0, 0, 1, 33 << 1]));
        assert!(FarmCodec::H265.is_parameter_set(&[0, 0, 1, 32 << 1]));
        assert!(!FarmCodec::H265.is_idr_nal(&[0, 0, 1, 1 << 1]));
    }

    #[test]
    fn key_includes_room_version_and_spec() {
        let room = room();
        let base = spec(30).key(room, 0);
        assert_ne!(base, spec(30).key(room, 1), "версия холста — часть ключа");
        assert_ne!(base, spec(30).key(RoomId::new(), 0), "комната — часть ключа");
        assert_ne!(base, spec(25).key(room, 0), "fps — часть ключа");
        assert_eq!(base, spec(30).key(room, 0), "ключ стабилен");
    }

    #[tokio::test]
    async fn viewer_rtp_counters_aggregate_per_group() {
        let farm = farm_with_test_spawner(8);
        let room = room();
        let key = spec(30).key(room, 0);
        let first = farm.acquire(key.clone(), (640, 360)).await.expect("acquire");
        let second = farm.acquire(key.clone(), (640, 360)).await.expect("acquire 2");
        first.note_rtp(3, 100);
        first.note_dropped(1);
        second.note_rtp(4, 200);
        let snapshot = farm.snapshot();
        assert_eq!(snapshot.len(), 1);
        assert_eq!(snapshot[0].viewers, 2, "два зрителя в группе");
        assert_eq!(snapshot[0].viewer_packets, 7, "RTP-пакеты суммируются по группе");
        assert_eq!(snapshot[0].viewer_bytes, 300, "байты суммируются по группе");
        assert_eq!(snapshot[0].viewer_dropped, 1, "потери зрителей суммируются");
        assert_eq!(first.rtp_snapshot(), (7, 300, 1), "снимок одинаков для всех зрителей группы");
    }

    #[tokio::test]
    async fn acquire_release_tracks_refcount_and_kills_group() {
        let farm = farm_with_test_spawner(8);
        let room = room();
        let key = spec(30).key(room, 0);
        let first = farm.acquire(key.clone(), (640, 360)).await.expect("acquire");
        assert_eq!(farm.group_count(), 1);
        assert_eq!(first.refcount(), 2, "зритель + энкодер");
        let second = farm.acquire(key.clone(), (640, 360)).await.expect("acquire 2");
        assert_eq!(farm.group_count(), 1, "тот же ключ — та же группа");
        assert_eq!(second.refcount(), 3);
        drop(second);
        assert_eq!(first.refcount(), 2, "один зритель остался");
        // Кадр доходит до группы, пока жив зритель.
        assert_eq!(farm.publish(room, &fake_frame(7)), 1, "кадр принят группой");
        drop(first);
        assert_eq!(farm.group_count(), 0, "refcount 1 (только энкодер) → группа умирает");
    }

    /// `P0`: после ухода последнего зрителя группа уничтожается **вместе с
    /// процессом** — реальный FFmpeg не должен пережить retire (баг живого
    /// прогона: 26 сирот с живыми stdin-пайпами).
    #[tokio::test]
    async fn retire_kills_encoder_process_group() {
        if !ffmpeg_available() {
            eprintln!("skipping: ffmpeg not available");
            return;
        }
        let _serial = FFMPEG_TEST_LOCK.lock().await;
        let farm = EncoderFarm::new();
        let room = room();
        let key = spec(30).key(room, 0);
        let viewer = farm.acquire(key.clone(), (640, 360)).await.expect("acquire");
        assert_eq!(farm.group_count(), 1);
        wait_until(|| ffmpeg_children() >= 1, "энкодер не поднялся").await;
        viewer.release();
        wait_until(
            || farm.group_count() == 0 && ffmpeg_children() == 0 && zombie_children() == 0,
            "после retire остался живой ffmpeg/зомби (утечка child)",
        )
        .await;
    }

    /// `P0`: гонка `acquire` на один ключ (3–10 сессий одновременно берут
    /// новую группу при re-key) не должна плодить дубли-процессы: ровно одна
    /// группа и ровно один энкодер, после ухода всех — ноль детей и ноль зомби.
    #[tokio::test(flavor = "multi_thread", worker_threads = 8)]
    async fn concurrent_acquires_share_one_encoder_process() {
        if !ffmpeg_available() {
            eprintln!("skipping: ffmpeg not available");
            return;
        }
        let _serial = FFMPEG_TEST_LOCK.lock().await;
        let farm = Arc::new(EncoderFarm::new());
        let room = room();
        let key = spec(30).key(room, 0);
        let mut handles = Vec::new();
        for _ in 0..8 {
            let farm = farm.clone();
            let key = key.clone();
            handles.push(tokio::spawn(async move { farm.acquire(key, (640, 360)).await }));
        }
        let mut viewers = Vec::new();
        for handle in handles {
            viewers.push(handle.await.expect("join").expect("acquire"));
        }
        assert_eq!(farm.group_count(), 1, "один ключ — ровно одна группа");
        wait_until(|| ffmpeg_children() == 1, "должен быть ровно один энкодер (не дубль)").await;
        for viewer in viewers {
            viewer.release();
        }
        wait_until(
            || farm.group_count() == 0 && ffmpeg_children() == 0 && zombie_children() == 0,
            "после ухода всех зрителей остались дети/зомби",
        )
        .await;
    }

    /// `P0`-страховка: если группа исчезла из мапы (например, гонка вставки
    /// или будущий баг), актор-сирота сам завершается по `Arc::strong_count == 1`
    /// и убивает child — вечной утечки ffmpeg быть не может.
    #[tokio::test]
    async fn orphaned_actor_self_terminates_and_kills_child() {
        let farm = farm_with_test_spawner(8);
        let room = room();
        let key = spec(30).key(room, 0);
        let viewer = farm.acquire(key.clone(), (640, 360)).await.expect("acquire");
        wait_until(|| children_with("cat") == 1, "фейковый энкодер не поднялся").await;
        // Имитация потери группы: убираем её из мапы, не убивая child.
        let group = farm.inner.groups.lock().unwrap().remove(&key).expect("группа в мапе");
        drop(viewer); // release: retire(key) → в мапе уже нет → no-op
        wait_until(
            || group.join.lock().unwrap().as_ref().is_some_and(|actor| actor.is_finished()),
            "актор-сирота не завершился сам",
        )
        .await;
        wait_until(|| children_with("cat") == 0, "актор-сирота не убил child").await;
    }

    /// `P0`: смена холста (`invalidate_room`) убивает старые группы вместе с
    /// процессами, а не просто помечает `cancel`.
    #[tokio::test]
    async fn invalidate_room_kills_encoder_processes() {
        if !ffmpeg_available() {
            eprintln!("skipping: ffmpeg not available");
            return;
        }
        let _serial = FFMPEG_TEST_LOCK.lock().await;
        let farm = EncoderFarm::new();
        let room = room();
        let viewer = farm.acquire(spec(30).key(room, 0), (640, 360)).await.expect("acquire");
        wait_until(|| ffmpeg_children() >= 1, "энкодер не поднялся").await;
        farm.invalidate_room(room);
        // Группа старой версии ушла из карты и её процесс убит.
        wait_until(
            || farm.group_count() == 0 && ffmpeg_children() == 0 && zombie_children() == 0,
            "invalidate_room оставил живой ffmpeg/зомби",
        )
        .await;
        drop(viewer);
    }

    #[tokio::test]
    async fn different_specs_produce_different_groups() {
        let farm = farm_with_test_spawner(8);
        let room = room();
        let low = farm.acquire(spec(30).key(room, 0), (640, 360)).await.expect("acquire");
        let mut high_spec = spec(30);
        high_spec.bitrate_kbps = 2000;
        let high = farm.acquire(high_spec.key(room, 0), (640, 360)).await.expect("acquire");
        assert_eq!(farm.group_count(), 2, "разный профиль — разные группы");
        assert_eq!(farm.publish(room, &fake_frame(1)), 2, "кадр уходит обеим группам");
        drop(high);
        drop(low);
        assert_eq!(farm.group_count(), 0);
    }

    #[tokio::test]
    async fn overloaded_when_limit_reached() {
        let farm = farm_with_test_spawner(1);
        let room = room();
        let _first = farm.acquire(spec(30).key(room, 0), (640, 360)).await.expect("acquire");
        let mut other = spec(30);
        other.bitrate_kbps = 2000;
        let error = farm.acquire(other.key(room, 0), (640, 360)).await.expect_err("лимит");
        assert_eq!(error, FarmError::Overloaded { limit: 1 });
    }

    #[tokio::test]
    async fn invalidate_room_bumps_version_and_closes_old_groups() {
        let farm = farm_with_test_spawner(8);
        let room = room();
        assert_eq!(farm.canvas_version(room), 0);
        let viewer = farm.acquire(spec(30).key(room, 0), (640, 360)).await.expect("acquire");
        farm.invalidate_room(room);
        assert_eq!(farm.canvas_version(room), 1);
        assert_eq!(farm.publish(room, &fake_frame(3)), 0, "старая группа не получает новые кадры");
        let error = farm.acquire(spec(30).key(room, 0), (640, 360)).await.expect_err("старая версия");
        assert_eq!(error, FarmError::Closed, "старый ключ закрыт, зритель переподключится");
        let fresh = farm.acquire(spec(30).key(room, 1), (640, 360)).await.expect("новая версия");
        assert_eq!(farm.publish(room, &fake_frame(3)), 1);
        drop(fresh);
        drop(viewer);
        assert_eq!(farm.group_count(), 0);
    }

    #[tokio::test]
    async fn keyframe_requests_are_group_scoped() {
        let farm = farm_with_test_spawner(8);
        let room = room();
        let viewer = farm.acquire(spec(30).key(room, 0), (640, 360)).await.expect("acquire");
        farm.request_keyframe(room);
        farm.request_keyframe(room);
        let stats = farm.snapshot();
        assert_eq!(stats.len(), 1);
        assert_eq!(stats[0].keyframe_requests, 2, "PLI/FIR копятся на группе");
        assert_eq!(stats[0].refcount, 2);
        drop(viewer);
    }

    #[tokio::test]
    async fn publish_dedups_frames_within_min_interval() {
        let farm = farm_with_test_spawner(8);
        let room = room();
        let viewer = farm.acquire(spec(1).key(room, 0), (640, 360)).await.expect("acquire");
        // fps=1 → min_interval 750 мс: второй кадр сразу — дедуп.
        assert_eq!(farm.publish(room, &fake_frame(1)), 1);
        assert_eq!(farm.publish(room, &fake_frame(2)), 0, "дубль в том же интервале отброшен");
        tokio::time::sleep(Duration::from_millis(900)).await;
        assert_eq!(farm.publish(room, &fake_frame(3)), 1, "следующий интервал принят");
        drop(viewer);
    }

    #[test]
    fn soak_budget_for_66_groups_is_arithmetic_not_absolute_rss() {
        let (leak, retention) = farm_soak_budget(FARM_SOAK_GROUPS);
        assert_eq!(leak, 66 * 1024 * 1024, "<=1 MiB leaked per sequential start/stop");
        assert_eq!(retention, 792 * 1024 * 1024, "<=12 MiB retained per live group");
        assert_eq!(farm_soak_budget(0), (0, 0));
        // The old plan's "579 MB absolute RSS" gate was allocator-dependent:
        // budgets must scale with groups, not with a fixed total.
        assert!(farm_soak_budget(1).0 < farm_soak_budget(2).0);
    }

    fn counting_spawner(counter: Arc<AtomicUsize>) -> Spawner {
        Arc::new(move |_spec: &EncoderSpec, _input: (u32, u32)| {
            counter.fetch_add(1, Ordering::SeqCst);
            let mut command = vks_common::process::safe_command("cat");
            command.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
            command.spawn()
        })
    }

    #[tokio::test]
    async fn subscribe_without_group_returns_none_and_does_not_spawn() {
        let spawns = Arc::new(AtomicUsize::new(0));
        let farm = EncoderFarm::with_spawner(counting_spawner(Arc::clone(&spawns)), 4);
        let room = room();
        assert!(farm.subscribe(room, FarmCodec::H264).is_none(), "no group -> no tap");
        assert_eq!(farm.group_count(), 0, "subscribe must not insert a hidden group");
        assert_eq!(spawns.load(Ordering::SeqCst), 0, "subscribe must never spawn FFmpeg");
        assert!(!farm.has_active_h264_group(room));
        assert_eq!(farm.active_group_count(room, FarmCodec::H264), 0);
    }

    #[tokio::test]
    async fn subscribe_reuses_existing_group_without_refcount_or_spawn() {
        let spawns = Arc::new(AtomicUsize::new(0));
        let farm = EncoderFarm::with_spawner(counting_spawner(Arc::clone(&spawns)), 4);
        let room = room();
        let key = spec(30).key(room, 0);
        let viewer = farm.acquire(key.clone(), (640, 360)).await.expect("acquire");
        assert_eq!(spawns.load(Ordering::SeqCst), 1);
        let before = farm.snapshot();
        assert!(farm.has_active_h264_group(room));
        assert_eq!(farm.active_group_count(room, FarmCodec::H264), 1);

        let tap = farm.subscribe(room, FarmCodec::H264).expect("existing group is subscribable");
        assert_eq!(tap.key, key);
        assert_eq!(spawns.load(Ordering::SeqCst), 1, "subscribe reuses the running encoder");
        assert_eq!(farm.group_count(), 1);
        let after = farm.snapshot();
        assert_eq!(after[0].refcount, before[0].refcount, "tap is non-owning (no refcount bump)");
        assert_eq!(after[0].viewers, before[0].viewers, "tap is not a viewer");

        drop(tap);
        drop(viewer);
    }

    #[tokio::test]
    async fn subscribe_filters_codec_and_stale_canvas_versions() {
        let farm = farm_with_test_spawner(8);
        let room = room();

        let h265 = EncoderSpec::canonical(FarmCodec::H265, 640, 360, 30, 800, "baseline", "3.1");
        let h265_key = h265.key(room, 0);
        let viewer = farm.acquire(h265_key.clone(), (640, 360)).await.expect("acquire h265");
        assert!(farm.subscribe(room, FarmCodec::H264).is_none(), "H.265 group is not an H.264 tap");
        assert!(!farm.has_active_h264_group(room));
        let h265_tap = farm.subscribe(room, FarmCodec::H265).expect("H.265 tap");
        assert_eq!(h265_tap.key, h265_key);
        drop(h265_tap);
        drop(viewer);

        let h264_key = spec(30).key(room, 0);
        let viewer = farm.acquire(h264_key, (640, 360)).await.expect("acquire h264");
        farm.invalidate_room(room);
        assert!(farm.subscribe(room, FarmCodec::H264).is_none(), "stale canvas group is not tapped");
        assert!(!farm.has_active_h264_group(room));
        drop(viewer);
    }

    #[tokio::test]
    async fn parameter_sets_are_cached_for_late_viewers() {
        // Два зрителя: второй приходит после первого кадра — получает кэш SPS/PPS.
        let farm = farm_with_test_spawner(8);
        let room = room();
        let key = spec(30).key(room, 0);
        let first = farm.acquire(key.clone(), (640, 360)).await.expect("acquire");
        let group = farm.inner.groups.lock().unwrap().get(&key).cloned().expect("group");
        *group.parameter_sets.lock().unwrap() = vec![Arc::from(&[0u8, 0, 1, 0x67, 1, 2][..])];
        let second = farm.acquire(key, (640, 360)).await.expect("acquire 2");
        assert_eq!(second.parameter_sets.len(), 1, "новый зритель получает SPS/PPS сразу");
        drop(second);
        drop(first);
    }

    #[test]
    fn canonical_spec_profile_matrix_smart_gop_and_vbv() {
        // Baseline (e.g. Polycom RealPresence Desktop)
        let baseline = EncoderSpec::canonical(FarmCodec::H264, 1280, 720, 30, 2048, "baseline", "3.1");
        assert_eq!(baseline.maxrate_kbps, 2355); // (2048 * 115) / 100
        assert_eq!(baseline.bufsize_kbps, 1024); // 2048 / 2
        assert_eq!(baseline.keyint, 60); // 30 * 2
        assert!(baseline.encoder_params.contains("slice-max-size=1180"));
        assert!(baseline.encoder_params.contains("deblock=0,0"));
        assert!(baseline.encoder_params.contains("cabac=0"));

        // High Profile (WebRTC)
        let high = EncoderSpec::canonical(FarmCodec::H264, 1280, 720, 30, 2048, "high", "4.1");
        assert!(high.encoder_params.contains("cabac=1"));
        assert!(high.encoder_params.contains("8x8dct=1"));
        assert!(high.encoder_params.contains("aq-mode=2"));
        assert!(high.encoder_params.contains("slice-max-size=1180"));

        // Content / Presentation Slide profile (1080p @ 15fps)
        let slide = EncoderSpec::canonical(FarmCodec::H264, 1920, 1080, 15, 1024, "baseline", "4.0");
        assert_eq!(slide.keyint, 30); // 15 * 2
        assert!(slide.encoder_params.contains("aq-mode=3"));
        assert!(slide.encoder_params.contains("slice-max-size=1180"));

        // H.265 profile
        let h265 = EncoderSpec::canonical(FarmCodec::H265, 1920, 1080, 30, 1500, "main", "4.1");
        assert!(h265.encoder_params.contains("sao=0:wpp=1"));
        assert_eq!(h265.maxrate_kbps, 1725);
        assert_eq!(h265.bufsize_kbps, 750);
    }
}
