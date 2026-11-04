//! Запись воспроизводимого медиаконтейнера: [`PlayableContainerWriter`], [`ContainerConfig`], [`ContainerError`].
//!
//! Пишет валидный ISO/IEC 14496-12 (ISOBMFF/MP4) файл с H.264 sample entry
//! `avc1` + `AVCDecoderConfigurationRecord` (`avcC`) и length-prefixed NALU
//! (4 байта) в `mdat` — по ISO/IEC 14496-15. Annex-B, который отдаёт
//! `AccessUnit`, конвертируется на входе;
//! SPS/PPS снимаются со start code и попадают в `avcC` как есть (emulation
//! prevention bytes не трогаются).
//!
//! Шкалы времени: видео — 90 000 тиков/с, аудио — частота дискретизации
//! (например 48 000). `stts` считается в тиках, `mdhd duration` = сумме
//! дельт. `mvhd`/`tkhd` остаются в миллисекундах (movie timescale 1000).
//!
//! Допущение: B-кадров нет (`ctts` не пишется). Production-профиль
//! zero-latency (`ffmpeg -tune zerolatency` в gateway_video) выдаёт только
//! I/P-кадры, и `AccessUnit` несёт один PTS
//! без DTS. Если в группу придёт энкодер с B-кадрами, writer обязан получить
//! DTS/ctts — иначе порядок сэмплов и `stts` будут неверными (T09b P2-3,
//! зафиксировано как допущение, а не скрытый дефект).
//!
//! Аудио — отдельный путь AAC/`mp4a`+`esds` из готового `AudioSpecificConfig`.
//! Живого AAC-источника в записи MCU нет, поэтому по умолчанию файл
//! video-only; аудиотрек пишется только когда реально есть
//! [`AudioConfig`] и аудио-сэмплы (пустой/фиктивный `mp4a` не создаётся).

use thiserror::Error;
use vks_common::annexb::{strip_start_code, trim_trailing_zeros};

/// Movie timescale (`mvhd`/`tkhd`): миллисекунды.
pub const MOVIE_TIMESCALE: u32 = 1_000;
/// Video media timescale: 90 kHz (тики H.264 в MP4).
pub const VIDEO_TIMESCALE: u32 = 90_000;
/// AAC-LC access unit: 1024 сэмпла на кадр.
pub const AAC_FRAME_SAMPLES: u32 = 1_024;

/// Кодек видео, который умеет писать контейнер.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum VideoCodec {
    /// H.264 (`avc1` + `avcC`).
    #[default]
    H264,
    /// H.265: пока не поддерживается (нужен `hvcC`), writer fail-closed.
    H265,
}

/// Конфигурация AAC-аудиотрека (`mp4a` + `esds`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioConfig {
    /// Частота дискретизации (mdhd timescale), например 48 000.
    pub sample_rate: u32,
    /// Число каналов (стерео = 2).
    pub channels: u16,
    /// AudioSpecificConfig, например `0x11 0x90` для AAC-LC 48 kHz stereo.
    pub asc: Vec<u8>,
}

impl AudioConfig {
    /// Конфигурация AAC-LC с готовым AudioSpecificConfig.
    #[must_use]
    pub fn aac_lc(sample_rate: u32, channels: u16, asc: impl Into<Vec<u8>>) -> Self {
        Self { sample_rate, channels, asc: asc.into() }
    }
}

/// Конфигурация медиаконтейнера.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContainerConfig {
    /// Ширина видео в пикселях (фактического выхода группы, не UI-профиля).
    pub width: u32,
    /// Высота видео в пикселях.
    pub height: u32,
    /// Кадров в секунду (для длительности последнего сэмпла и `tkhd`).
    pub fps: u32,
    /// Кодек видео (writer поддерживает только H.264).
    pub video_codec: VideoCodec,
    /// AAC-аудиотрек; `None` — video-only (по умолчанию).
    pub audio: Option<AudioConfig>,
    /// Максимальный размер контейнера в байтах (квота).
    pub max_size_bytes: usize,
}

impl Default for ContainerConfig {
    fn default() -> Self {
        Self {
            width: 1280,
            height: 720,
            fps: 30,
            video_codec: VideoCodec::H264,
            audio: None,
            max_size_bytes: 50 * 1024 * 1024, // 50 MB default quota
        }
    }
}

/// Ошибки записи медиаконтейнера.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ContainerError {
    /// Достигнут лимит свободного места / квоты хранилища (Storage full).
    #[error("хранилище переполнено: превышен лимит {max} байт")]
    StorageFull {
        /// Квота в байтах.
        max: usize,
    },
    /// Контейнер уже финализирован.
    #[error("контейнер уже финализирован")]
    AlreadyFinalized,
    /// Некорректная (назад идущая) временная метка.
    #[error("некорректный timestamp {0} us")]
    InvalidTimestamp(u64),
    /// Кодек не H.264: `avc1`/`avcC` для него невалиден.
    #[error("видеокодек не поддерживается (нужен H.264/avcC)")]
    UnsupportedCodec,
    /// Аудио-сэмпл без [`AudioConfig`]: нельзя писать `mp4a` без `esds`.
    #[error("аудио не сконфигурировано (нужен AAC AudioSpecificConfig)")]
    UnsupportedAudio,
    /// Пустой контейнер: не сохраняем «валидный» файл без единого сэмпла.
    #[error("контейнер пуст: нет ни одного видеосэмпла")]
    Empty,
    /// Нет SPS/PPS — `avcC` построить нельзя.
    #[error("нет SPS/PPS для avcC")]
    MissingParameterSets,
    /// SPS/PPS изменились посреди записи: нужен новый файл, не подмена `avcC`.
    #[error("SPS/PPS изменились посреди записи")]
    ParameterSetsChanged,
    /// SPS/PPS слишком короткие/битые для чтения profile/level.
    #[error("некорректные SPS/PPS")]
    InvalidParameterSets,
    /// Первый видео-сэмпл не keyframe: декодер начнёт с середины GOP.
    #[error("первый видеосэмпл должен быть keyframe (IDR)")]
    FirstSampleNotKeyframe,
    /// За запись не было ни одного keyframe.
    #[error("в записи нет keyframe")]
    NoKeyframe,
    /// Сэмпл/смещение не влезает в 32-битные поля ISO (`stco`/`stsz`/box size).
    #[error("контейнер превышает 32-битные пределы ISO BMFF")]
    TooLarge,
    /// Двухпроходный `moov` дал разные размеры: смещения были бы битыми.
    #[error("размер moov нестабилен между проходами")]
    MoovSizeMismatch,
    /// Нулевые/некорректные width/height/fps или аудиопараметры.
    #[error("некорректная конфигурация контейнера")]
    InvalidConfig,
    /// В Annex-B сэмпле нет ни одного NAL.
    #[error("пустой Annex-B сэмпл")]
    EmptySample,
}

/// Один записанный видео-сэмпл (в порядке mdat).
#[derive(Debug, Clone)]
struct VideoSample {
    pts_us: u64,
    pts_ticks: u32,
    size: usize,
    offset: usize,
    is_keyframe: bool,
}

/// Один записанный аудио-сэмпл (сырой AAC access unit, без ADTS).
#[derive(Debug, Clone)]
struct AudioSample {
    pts_us: u64,
    size: usize,
    offset: usize,
}

/// Параметры `moov`, одинаковые для обоих проходов.
struct TrackLayout<'a> {
    avcc: &'a [u8],
    video_deltas: &'a [u32],
    video_duration_ticks: u32,
    video_duration_ms: u32,
    audio: Option<AudioTrackLayout<'a>>,
}

struct AudioTrackLayout<'a> {
    asc: &'a [u8],
    sample_rate: u32,
    deltas: &'a [u32],
    duration_ticks: u32,
    duration_ms: u32,
}

/// Записыватель воспроизводимого ISOBMFF (MP4) контейнера.
pub struct PlayableContainerWriter {
    config: ContainerConfig,
    video_samples: Vec<VideoSample>,
    audio_samples: Vec<AudioSample>,
    media_data: Vec<u8>,
    sps: Option<Vec<u8>>,
    pps: Option<Vec<u8>>,
    finalized: bool,
}

impl PlayableContainerWriter {
    /// Создаёт новый записыватель контейнера с заданной конфигурацией.
    pub fn new(config: ContainerConfig) -> Self {
        Self {
            config,
            video_samples: Vec::new(),
            audio_samples: Vec::new(),
            media_data: Vec::new(),
            sps: None,
            pps: None,
            finalized: false,
        }
    }

    /// Записывает видео-сэмпл (один access unit в Annex-B или уже
    /// length-prefixed виде).
    ///
    /// `pts_us` — время показа в микросекундах от старта записи; writer сам
    /// переводит его в 90 kHz-тики без промежуточного округления в мс.
    /// Первый сэмпл обязан быть keyframe.
    pub fn write_video_sample(&mut self, data: &[u8], pts_us: u64, is_keyframe: bool) -> Result<(), ContainerError> {
        self.ensure_writable()?;
        if self.config.video_codec != VideoCodec::H264 {
            return Err(ContainerError::UnsupportedCodec);
        }
        if self.video_samples.is_empty() && !is_keyframe {
            return Err(ContainerError::FirstSampleNotKeyframe);
        }
        let (sample, nals) = normalize_video_sample(data)?;
        self.check_capacity(sample.len())?;
        if let Some(last) = self.video_samples.last() {
            if pts_us < last.pts_us {
                return Err(ContainerError::InvalidTimestamp(pts_us));
            }
        }
        let pts_ticks = pts_us_to_ticks(pts_us)?;
        let (sps, pps) = parameter_sets_from_nals(&nals);
        self.apply_parameter_sets(sps, pps)?;
        let offset = self.media_data.len();
        let size = sample.len();
        self.media_data.extend_from_slice(&sample);
        self.video_samples.push(VideoSample { pts_us, pts_ticks, size, offset, is_keyframe });
        Ok(())
    }

    /// Записывает аудио-сэмпл (сырой AAC access unit, без ADTS-заголовка).
    ///
    /// Требует [`ContainerConfig::audio`]; без него возвращает
    /// [`ContainerError::UnsupportedAudio`] (не подделываем `mp4a`).
    pub fn write_audio_sample(&mut self, data: &[u8], pts_us: u64) -> Result<(), ContainerError> {
        self.ensure_writable()?;
        let Some(audio) = self.config.audio.as_ref() else {
            return Err(ContainerError::UnsupportedAudio);
        };
        if audio.sample_rate == 0 || audio.channels == 0 || audio.asc.is_empty() {
            return Err(ContainerError::InvalidConfig);
        }
        if data.is_empty() {
            return Err(ContainerError::EmptySample);
        }
        self.check_capacity(data.len())?;
        if let Some(last) = self.audio_samples.last() {
            if pts_us < last.pts_us {
                return Err(ContainerError::InvalidTimestamp(pts_us));
            }
        }
        let offset = self.media_data.len();
        let size = data.len();
        self.media_data.extend_from_slice(data);
        self.audio_samples.push(AudioSample { pts_us, size, offset });
        Ok(())
    }

    /// Задаёт SPS/PPS заранее (snapshot группы фермы). Start code снимается;
    /// emulation prevention bytes сохраняются. Повторный вызов с другими
    /// байтами — [`ContainerError::ParameterSetsChanged`].
    pub fn set_parameter_sets(&mut self, sets: &[&[u8]]) -> Result<(), ContainerError> {
        self.ensure_writable()?;
        let mut sps = None;
        let mut pps = None;
        for raw in sets {
            let nal = trim_trailing_zeros(strip_start_code(raw));
            if nal.is_empty() {
                continue;
            }
            match nal[0] & 0x1f {
                7 if sps.is_none() => sps = Some(nal.to_vec()),
                8 if pps.is_none() => pps = Some(nal.to_vec()),
                _ => {}
            }
        }
        self.apply_parameter_sets(sps, pps)
    }

    /// Число записанных сэмплов (видео + аудио).
    pub fn sample_count(&self) -> usize {
        self.video_samples.len() + self.audio_samples.len()
    }

    /// Число записанных видео-сэмплов.
    pub fn video_sample_count(&self) -> usize {
        self.video_samples.len()
    }

    /// Число записанных аудио-сэмплов.
    pub fn audio_sample_count(&self) -> usize {
        self.audio_samples.len()
    }

    /// Текущий размер накопленных медиаданных в байтах.
    pub fn current_size(&self) -> usize {
        self.media_data.len()
    }

    /// Финализирует контейнер и возвращает байты готового MP4 файла.
    pub fn finalize(&mut self) -> Result<Vec<u8>, ContainerError> {
        if self.finalized {
            return Err(ContainerError::AlreadyFinalized);
        }
        self.finalized = true;
        self.validate_config()?;
        if self.config.video_codec != VideoCodec::H264 {
            return Err(ContainerError::UnsupportedCodec);
        }
        if self.video_samples.is_empty() {
            return Err(ContainerError::Empty);
        }
        if !self.video_samples[0].is_keyframe {
            return Err(ContainerError::FirstSampleNotKeyframe);
        }
        if !self.video_samples.iter().any(|sample| sample.is_keyframe) {
            return Err(ContainerError::NoKeyframe);
        }
        let (Some(sps), Some(pps)) = (self.sps.as_deref(), self.pps.as_deref()) else {
            return Err(ContainerError::MissingParameterSets);
        };
        let avcc = build_avcc(sps, pps)?;

        let frame_ticks = video_frame_ticks(self.config.fps)?;
        let video_deltas = video_stts_deltas(&self.video_samples, frame_ticks);
        let video_duration_ticks = checked_sum_u32(&video_deltas)?;
        let video_duration_ms = ticks_to_ms(video_duration_ticks, VIDEO_TIMESCALE);

        let has_audio = self.config.audio.is_some() && !self.audio_samples.is_empty();
        let audio_deltas = if has_audio { vec![AAC_FRAME_SAMPLES; self.audio_samples.len()] } else { Vec::new() };
        let audio_duration_ticks_u64 = u64::from(AAC_FRAME_SAMPLES) * self.audio_samples.len() as u64;
        let audio_duration_ticks = u32::try_from(audio_duration_ticks_u64).map_err(|_| ContainerError::TooLarge)?;
        let audio_layout = if has_audio {
            let audio = self.config.audio.as_ref().expect("has_audio checked");
            Some(AudioTrackLayout {
                asc: &audio.asc,
                sample_rate: audio.sample_rate,
                deltas: &audio_deltas,
                duration_ticks: audio_duration_ticks,
                duration_ms: ticks_to_ms(audio_duration_ticks, audio.sample_rate),
            })
        } else {
            None
        };
        let layout = TrackLayout {
            avcc: &avcc,
            video_deltas: &video_deltas,
            video_duration_ticks,
            video_duration_ms,
            audio: audio_layout,
        };

        let ftyp_box = self.build_ftyp_box()?;
        let ftyp_len = ftyp_box.len();
        let video_sample_count = self.video_samples.len();
        let audio_sample_count = self.audio_samples.len();

        // Pass 1: размер moov с фиктивными смещениями. Состав боксов от
        // смещений не зависит — pass 2 обязан дать тот же размер.
        let dummy_video_offsets = vec![0usize; video_sample_count];
        let dummy_audio_offsets = vec![0usize; audio_sample_count];
        let dummy_moov = self.build_moov(&layout, &dummy_video_offsets, &dummy_audio_offsets)?;
        let moov_len = dummy_moov.len().checked_add(8).ok_or(ContainerError::TooLarge)?;

        // 2. Начало полезной нагрузки mdat в результирующем файле
        let mdat_payload_offset =
            ftyp_len.checked_add(moov_len).and_then(|value| value.checked_add(8)).ok_or(ContainerError::TooLarge)?;
        let mdat_end = mdat_payload_offset.checked_add(self.media_data.len()).ok_or(ContainerError::TooLarge)?;
        u32::try_from(mdat_end).map_err(|_| ContainerError::TooLarge)?;

        // Pass 2: точные смещения чанков в файле.
        let real_video_offsets: Vec<usize> =
            self.video_samples.iter().map(|sample| mdat_payload_offset + sample.offset).collect();
        let real_audio_offsets: Vec<usize> =
            self.audio_samples.iter().map(|sample| mdat_payload_offset + sample.offset).collect();

        let mut output = Vec::with_capacity(mdat_end);
        output.extend_from_slice(&ftyp_box);
        let moov_payload = self.build_moov(&layout, &real_video_offsets, &real_audio_offsets)?;
        if moov_payload.len() != dummy_moov.len() {
            return Err(ContainerError::MoovSizeMismatch);
        }
        write_box(&mut output, b"moov", &moov_payload)?;
        write_box(&mut output, b"mdat", &self.media_data)?;
        if output.len() > self.config.max_size_bytes {
            return Err(ContainerError::StorageFull { max: self.config.max_size_bytes });
        }
        Ok(output)
    }

    fn ensure_writable(&self) -> Result<(), ContainerError> {
        if self.finalized {
            Err(ContainerError::AlreadyFinalized)
        } else {
            Ok(())
        }
    }

    fn validate_config(&self) -> Result<(), ContainerError> {
        if self.config.width == 0
            || self.config.height == 0
            || self.config.fps == 0
            || self.config.width > u32::from(u16::MAX)
            || self.config.height > u32::from(u16::MAX)
        {
            return Err(ContainerError::InvalidConfig);
        }
        if let Some(audio) = self.config.audio.as_ref() {
            if audio.sample_rate == 0
                || audio.sample_rate > u32::from(u16::MAX)
                || audio.channels == 0
                || audio.asc.is_empty()
            {
                return Err(ContainerError::InvalidConfig);
            }
        }
        Ok(())
    }

    fn apply_parameter_sets(&mut self, sps: Option<Vec<u8>>, pps: Option<Vec<u8>>) -> Result<(), ContainerError> {
        if let Some(sps) = sps {
            match &self.sps {
                Some(existing) if existing != &sps => return Err(ContainerError::ParameterSetsChanged),
                Some(_) => {}
                None => self.sps = Some(sps),
            }
        }
        if let Some(pps) = pps {
            match &self.pps {
                Some(existing) if existing != &pps => return Err(ContainerError::ParameterSetsChanged),
                Some(_) => {}
                None => self.pps = Some(pps),
            }
        }
        Ok(())
    }

    fn check_capacity(&self, additional: usize) -> Result<(), ContainerError> {
        self.ensure_writable()?;
        // Таблицы stbl растут с числом сэмплов: ~16 байт/сэмпл на stsz+stco
        // плюс запас; финальная проверка в `finalize` — точная.
        let overhead = (self.video_samples.len() + self.audio_samples.len()).saturating_mul(16).saturating_add(8_192);
        let total = self.media_data.len().saturating_add(additional).saturating_add(overhead);
        if total > self.config.max_size_bytes {
            return Err(ContainerError::StorageFull { max: self.config.max_size_bytes });
        }
        Ok(())
    }

    fn build_ftyp_box(&self) -> Result<Vec<u8>, ContainerError> {
        let mut ftyp_payload = Vec::new();
        ftyp_payload.extend_from_slice(b"isom"); // Major brand
        ftyp_payload.extend_from_slice(&0x0000_0200u32.to_be_bytes()); // Minor version
        ftyp_payload.extend_from_slice(b"isom"); // Compatible brands
        ftyp_payload.extend_from_slice(b"iso2");
        ftyp_payload.extend_from_slice(b"avc1");
        ftyp_payload.extend_from_slice(b"mp41");
        let mut box_buf = Vec::new();
        write_box(&mut box_buf, b"ftyp", &ftyp_payload)?;
        Ok(box_buf)
    }

    fn build_moov(
        &self,
        layout: &TrackLayout<'_>,
        video_offsets: &[usize],
        audio_offsets: &[usize],
    ) -> Result<Vec<u8>, ContainerError> {
        let mut moov = Vec::new();
        let movie_duration_ms =
            layout.video_duration_ms.max(layout.audio.as_ref().map_or(0, |audio| audio.duration_ms));
        let next_track_id = if layout.audio.is_some() { 3u32 } else { 2u32 };

        // 2.1 'mvhd' (Movie Header)
        let mut mvhd = Vec::new();
        mvhd.extend_from_slice(&[0, 0, 0, 0]); // Version & flags
        mvhd.extend_from_slice(&0u32.to_be_bytes()); // Creation time
        mvhd.extend_from_slice(&0u32.to_be_bytes()); // Modification time
        mvhd.extend_from_slice(&MOVIE_TIMESCALE.to_be_bytes());
        mvhd.extend_from_slice(&movie_duration_ms.to_be_bytes());
        mvhd.extend_from_slice(&0x0001_0000u32.to_be_bytes()); // Rate 1.0
        mvhd.extend_from_slice(&0x0100u16.to_be_bytes()); // Volume 1.0
        mvhd.extend_from_slice(&[0u8; 10]); // Reserved
        extend_identity_matrix(&mut mvhd);
        mvhd.extend_from_slice(&[0u8; 24]); // Pre-defined
        mvhd.extend_from_slice(&next_track_id.to_be_bytes());
        write_box(&mut moov, b"mvhd", &mvhd)?;

        // 2.2 'trak' Video (Track ID 1)
        let video_trak = self.build_video_trak(layout, video_offsets)?;
        write_box(&mut moov, b"trak", &video_trak)?;

        // 2.3 'trak' Audio (Track ID 2) — только при реальных AAC-сэмплах.
        if let Some(audio) = layout.audio.as_ref() {
            let audio_trak = self.build_audio_trak(audio, audio_offsets)?;
            write_box(&mut moov, b"trak", &audio_trak)?;
        }

        Ok(moov)
    }

    fn build_video_trak(&self, layout: &TrackLayout<'_>, offsets: &[usize]) -> Result<Vec<u8>, ContainerError> {
        let mut trak = Vec::new();

        // tkhd
        let mut tkhd = Vec::new();
        tkhd.extend_from_slice(&[0, 0, 0, 0x07]); // Enabled, in movie, in preview
        tkhd.extend_from_slice(&0u32.to_be_bytes());
        tkhd.extend_from_slice(&0u32.to_be_bytes());
        tkhd.extend_from_slice(&1u32.to_be_bytes()); // Track ID 1
        tkhd.extend_from_slice(&0u32.to_be_bytes());
        tkhd.extend_from_slice(&layout.video_duration_ms.to_be_bytes());
        tkhd.extend_from_slice(&[0u8; 8]);
        tkhd.extend_from_slice(&0u16.to_be_bytes()); // Layer
        tkhd.extend_from_slice(&0u16.to_be_bytes()); // Alternate group
        tkhd.extend_from_slice(&0u16.to_be_bytes()); // Volume
        tkhd.extend_from_slice(&0u16.to_be_bytes());
        extend_identity_matrix(&mut tkhd);
        tkhd.extend_from_slice(&(self.config.width << 16).to_be_bytes()); // Width 16.16 fixed
        tkhd.extend_from_slice(&(self.config.height << 16).to_be_bytes()); // Height 16.16 fixed
        write_box(&mut trak, b"tkhd", &tkhd)?;

        // mdia
        let mut mdia = Vec::new();
        let mut mdhd = Vec::new();
        mdhd.extend_from_slice(&[0, 0, 0, 0]);
        mdhd.extend_from_slice(&0u32.to_be_bytes());
        mdhd.extend_from_slice(&0u32.to_be_bytes());
        mdhd.extend_from_slice(&VIDEO_TIMESCALE.to_be_bytes());
        mdhd.extend_from_slice(&layout.video_duration_ticks.to_be_bytes());
        mdhd.extend_from_slice(&0x55c4u16.to_be_bytes()); // Language 'und'
        mdhd.extend_from_slice(&0u16.to_be_bytes());
        write_box(&mut mdia, b"mdhd", &mdhd)?;

        let mut hdlr = Vec::new();
        hdlr.extend_from_slice(&[0, 0, 0, 0]);
        hdlr.extend_from_slice(&0u32.to_be_bytes());
        hdlr.extend_from_slice(b"vide"); // Handler type
        hdlr.extend_from_slice(&[0u8; 12]);
        hdlr.extend_from_slice(b"VideoHandler\0");
        write_box(&mut mdia, b"hdlr", &hdlr)?;

        // minf
        let mut minf = Vec::new();
        let mut vmhd = Vec::new();
        vmhd.extend_from_slice(&[0, 0, 0, 1]); // Flags = 1
        vmhd.extend_from_slice(&[0u8; 8]);
        write_box(&mut minf, b"vmhd", &vmhd)?;
        write_dinf(&mut minf)?;

        // stbl
        let mut stbl = Vec::new();
        let mut stsd = Vec::new();
        stsd.extend_from_slice(&[0, 0, 0, 0]);
        stsd.extend_from_slice(&1u32.to_be_bytes());
        let mut avc1 = Vec::new();
        avc1.extend_from_slice(&[0u8; 6]); // Reserved
        avc1.extend_from_slice(&1u16.to_be_bytes()); // Data ref index
        avc1.extend_from_slice(&[0u8; 16]);
        avc1.extend_from_slice(&(self.config.width as u16).to_be_bytes());
        avc1.extend_from_slice(&(self.config.height as u16).to_be_bytes());
        avc1.extend_from_slice(&0x0048_0000u32.to_be_bytes()); // 72 dpi
        avc1.extend_from_slice(&0x0048_0000u32.to_be_bytes());
        avc1.extend_from_slice(&0u32.to_be_bytes());
        avc1.extend_from_slice(&1u16.to_be_bytes()); // Frame count
        avc1.extend_from_slice(&[0u8; 32]); // Compressor name
        avc1.extend_from_slice(&0x0018u16.to_be_bytes()); // Depth 24
        avc1.extend_from_slice(&0xFFFFu16.to_be_bytes()); // pre_defined = -1
        write_box(&mut avc1, b"avcC", layout.avcc)?;
        write_box(&mut stsd, b"avc1", &avc1)?;
        write_box(&mut stbl, b"stsd", &stsd)?;

        let stts = build_stts(layout.video_deltas)?;
        write_box(&mut stbl, b"stts", &stts)?;

        let keyframes: Vec<u32> = self
            .video_samples
            .iter()
            .enumerate()
            .filter(|(_, sample)| sample.is_keyframe)
            .map(|(index, _)| u32::try_from(index + 1).map_err(|_| ContainerError::TooLarge))
            .collect::<Result<_, _>>()?;
        let stss = build_stss(&keyframes)?;
        write_box(&mut stbl, b"stss", &stss)?;

        let stsc = build_stsc(u32::try_from(self.video_samples.len()).map_err(|_| ContainerError::TooLarge)?)?;
        write_box(&mut stbl, b"stsc", &stsc)?;

        let sizes: Vec<usize> = self.video_samples.iter().map(|sample| sample.size).collect();
        let stsz = build_stsz(&sizes)?;
        write_box(&mut stbl, b"stsz", &stsz)?;

        let stco = build_stco(offsets)?;
        write_box(&mut stbl, b"stco", &stco)?;

        write_box(&mut minf, b"stbl", &stbl)?;
        write_box(&mut mdia, b"minf", &minf)?;
        write_box(&mut trak, b"mdia", &mdia)?;
        Ok(trak)
    }

    fn build_audio_trak(&self, audio: &AudioTrackLayout<'_>, offsets: &[usize]) -> Result<Vec<u8>, ContainerError> {
        let mut trak = Vec::new();

        // tkhd
        let mut tkhd = Vec::new();
        tkhd.extend_from_slice(&[0, 0, 0, 0x07]);
        tkhd.extend_from_slice(&0u32.to_be_bytes());
        tkhd.extend_from_slice(&0u32.to_be_bytes());
        tkhd.extend_from_slice(&2u32.to_be_bytes()); // Track ID 2
        tkhd.extend_from_slice(&0u32.to_be_bytes());
        tkhd.extend_from_slice(&audio.duration_ms.to_be_bytes());
        tkhd.extend_from_slice(&[0u8; 8]);
        tkhd.extend_from_slice(&0u16.to_be_bytes());
        tkhd.extend_from_slice(&0u16.to_be_bytes());
        tkhd.extend_from_slice(&0x0100u16.to_be_bytes()); // Full audio volume
        tkhd.extend_from_slice(&0u16.to_be_bytes());
        extend_identity_matrix(&mut tkhd);
        tkhd.extend_from_slice(&0u32.to_be_bytes()); // Width 0 for audio
        tkhd.extend_from_slice(&0u32.to_be_bytes()); // Height 0
        write_box(&mut trak, b"tkhd", &tkhd)?;

        // mdia
        let mut mdia = Vec::new();
        let mut mdhd = Vec::new();
        mdhd.extend_from_slice(&[0, 0, 0, 0]);
        mdhd.extend_from_slice(&0u32.to_be_bytes());
        mdhd.extend_from_slice(&0u32.to_be_bytes());
        mdhd.extend_from_slice(&audio.sample_rate.to_be_bytes());
        mdhd.extend_from_slice(&audio.duration_ticks.to_be_bytes());
        mdhd.extend_from_slice(&0x55c4u16.to_be_bytes());
        mdhd.extend_from_slice(&0u16.to_be_bytes());
        write_box(&mut mdia, b"mdhd", &mdhd)?;

        let mut hdlr = Vec::new();
        hdlr.extend_from_slice(&[0, 0, 0, 0]);
        hdlr.extend_from_slice(&0u32.to_be_bytes());
        hdlr.extend_from_slice(b"soun"); // Sound handler
        hdlr.extend_from_slice(&[0u8; 12]);
        hdlr.extend_from_slice(b"SoundHandler\0");
        write_box(&mut mdia, b"hdlr", &hdlr)?;

        let mut minf = Vec::new();
        let mut smhd = Vec::new();
        smhd.extend_from_slice(&[0, 0, 0, 0]);
        smhd.extend_from_slice(&0u16.to_be_bytes()); // Balance
        smhd.extend_from_slice(&0u16.to_be_bytes());
        write_box(&mut minf, b"smhd", &smhd)?;
        write_dinf(&mut minf)?;

        let mut stbl = Vec::new();
        let mut stsd = Vec::new();
        stsd.extend_from_slice(&[0, 0, 0, 0]);
        stsd.extend_from_slice(&1u32.to_be_bytes());
        let mut mp4a = Vec::new();
        mp4a.extend_from_slice(&[0u8; 6]);
        mp4a.extend_from_slice(&1u16.to_be_bytes());
        mp4a.extend_from_slice(&[0u8; 8]);
        let channels = self.config.audio.as_ref().expect("audio layout requires config").channels;
        mp4a.extend_from_slice(&channels.to_be_bytes());
        mp4a.extend_from_slice(&16u16.to_be_bytes()); // 16-bit
        mp4a.extend_from_slice(&0u32.to_be_bytes());
        mp4a.extend_from_slice(&(audio.sample_rate << 16).to_be_bytes());
        let esds = build_esds(audio.asc);
        write_box(&mut mp4a, b"esds", &esds)?;
        write_box(&mut stsd, b"mp4a", &mp4a)?;
        write_box(&mut stbl, b"stsd", &stsd)?;

        let stts = build_stts(audio.deltas)?;
        write_box(&mut stbl, b"stts", &stts)?;
        let stsc = build_stsc(u32::try_from(self.audio_samples.len()).map_err(|_| ContainerError::TooLarge)?)?;
        write_box(&mut stbl, b"stsc", &stsc)?;
        let sizes: Vec<usize> = self.audio_samples.iter().map(|sample| sample.size).collect();
        let stsz = build_stsz(&sizes)?;
        write_box(&mut stbl, b"stsz", &stsz)?;
        let stco = build_stco(offsets)?;
        write_box(&mut stbl, b"stco", &stco)?;

        write_box(&mut minf, b"stbl", &stbl)?;
        write_box(&mut mdia, b"minf", &minf)?;
        write_box(&mut trak, b"mdia", &mdia)?;
        Ok(trak)
    }
}

fn extend_identity_matrix(buf: &mut Vec<u8>) {
    let matrix: [u32; 9] = [0x0001_0000, 0, 0, 0, 0x0001_0000, 0, 0, 0, 0x4000_0000];
    for value in matrix {
        buf.extend_from_slice(&value.to_be_bytes());
    }
}

fn write_dinf(minf: &mut Vec<u8>) -> Result<(), ContainerError> {
    let mut dinf = Vec::new();
    let mut dref = Vec::new();
    dref.extend_from_slice(&[0, 0, 0, 0]);
    dref.extend_from_slice(&1u32.to_be_bytes()); // 1 entry
    let mut url_box = Vec::new();
    url_box.extend_from_slice(&[0, 0, 0, 1]); // Self-contained flag
    write_box(&mut dref, b"url ", &url_box)?;
    write_box(&mut dinf, b"dref", &dref)?;
    write_box(minf, b"dinf", &dinf)?;
    Ok(())
}

fn build_stts(deltas: &[u32]) -> Result<Vec<u8>, ContainerError> {
    let mut buf = Vec::new();
    buf.extend_from_slice(&[0, 0, 0, 0]); // version & flags
    let mut rle: Vec<(u32, u32)> = Vec::new();
    for &delta in deltas {
        if delta == 0 {
            continue;
        }
        if let Some(last) = rle.last_mut() {
            if last.1 == delta {
                last.0 = last.0.checked_add(1).ok_or(ContainerError::TooLarge)?;
                continue;
            }
        }
        rle.push((1, delta));
    }
    buf.extend_from_slice(&u32::try_from(rle.len()).map_err(|_| ContainerError::TooLarge)?.to_be_bytes());
    for (count, delta) in rle {
        buf.extend_from_slice(&count.to_be_bytes());
        buf.extend_from_slice(&delta.to_be_bytes());
    }
    Ok(buf)
}

fn build_stss(keyframe_indices_1based: &[u32]) -> Result<Vec<u8>, ContainerError> {
    let mut buf = Vec::new();
    buf.extend_from_slice(&[0, 0, 0, 0]);
    buf.extend_from_slice(
        &u32::try_from(keyframe_indices_1based.len()).map_err(|_| ContainerError::TooLarge)?.to_be_bytes(),
    );
    for &index in keyframe_indices_1based {
        buf.extend_from_slice(&index.to_be_bytes());
    }
    Ok(buf)
}

fn build_stsc(sample_count: u32) -> Result<Vec<u8>, ContainerError> {
    let mut buf = Vec::new();
    buf.extend_from_slice(&[0, 0, 0, 0]);
    if sample_count == 0 {
        buf.extend_from_slice(&0u32.to_be_bytes());
    } else {
        buf.extend_from_slice(&1u32.to_be_bytes()); // 1 entry
        buf.extend_from_slice(&1u32.to_be_bytes()); // first_chunk = 1
        buf.extend_from_slice(&1u32.to_be_bytes()); // samples_per_chunk = 1
        buf.extend_from_slice(&1u32.to_be_bytes()); // sample_description_index = 1
    }
    Ok(buf)
}

fn build_stsz(sizes: &[usize]) -> Result<Vec<u8>, ContainerError> {
    let mut buf = Vec::new();
    buf.extend_from_slice(&[0, 0, 0, 0]);
    buf.extend_from_slice(&0u32.to_be_bytes()); // variable sizes
    buf.extend_from_slice(&u32::try_from(sizes.len()).map_err(|_| ContainerError::TooLarge)?.to_be_bytes());
    for &size in sizes {
        buf.extend_from_slice(&u32::try_from(size).map_err(|_| ContainerError::TooLarge)?.to_be_bytes());
    }
    Ok(buf)
}

fn build_stco(offsets: &[usize]) -> Result<Vec<u8>, ContainerError> {
    let mut buf = Vec::new();
    buf.extend_from_slice(&[0, 0, 0, 0]);
    buf.extend_from_slice(&u32::try_from(offsets.len()).map_err(|_| ContainerError::TooLarge)?.to_be_bytes());
    for &offset in offsets {
        buf.extend_from_slice(&u32::try_from(offset).map_err(|_| ContainerError::TooLarge)?.to_be_bytes());
    }
    Ok(buf)
}

/// `AVCDecoderConfigurationRecord` (ISO/IEC 14496-15 §5.3.3.1).
fn build_avcc(sps: &[u8], pps: &[u8]) -> Result<Vec<u8>, ContainerError> {
    if sps.len() < 4 || pps.is_empty() {
        return Err(ContainerError::InvalidParameterSets);
    }
    let sps_len = u16::try_from(sps.len()).map_err(|_| ContainerError::InvalidParameterSets)?;
    let pps_len = u16::try_from(pps.len()).map_err(|_| ContainerError::InvalidParameterSets)?;
    let mut avcc = Vec::with_capacity(8 + sps.len() + 3 + pps.len() + 4);
    avcc.push(1); // configurationVersion
    avcc.push(sps[1]); // AVCProfileIndication
    avcc.push(sps[2]); // profile_compatibility
    avcc.push(sps[3]); // AVCLevelIndication
    avcc.push(0xFF); // reserved 6 bits + lengthSizeMinusOne = 3 (4-byte lengths)
    avcc.push(0xE0 | 1); // reserved 3 bits + numOfSequenceParameterSets = 1
    avcc.extend_from_slice(&sps_len.to_be_bytes());
    avcc.extend_from_slice(sps);
    avcc.push(1); // numOfPictureParameterSets
    avcc.extend_from_slice(&pps_len.to_be_bytes());
    avcc.extend_from_slice(pps);
    // High profile family требует extension-поля (ffmpeg пишет их же):
    // chroma_format=1 (4:2:0), bit_depth_luma/chroma_minus8=0, numOfSpsExt=0.
    if matches!(sps[1], 100 | 110 | 122 | 144) {
        avcc.extend_from_slice(&[0xFD, 0xF8, 0xF8, 0x00]);
    }
    Ok(avcc)
}

/// `esds` (ISO/IEC 14496-1) с DecoderSpecificInfo = AudioSpecificConfig.
fn build_esds(asc: &[u8]) -> Vec<u8> {
    let decoder_specific = descriptor(0x05, asc);
    let mut decoder_config = Vec::with_capacity(13 + decoder_specific.len());
    decoder_config.push(0x40); // objectTypeIndication: MPEG-4 Audio (AAC)
    decoder_config.push(0x15); // streamType audio (0x05 << 2) | upstream 0 | reserved 1
    decoder_config.extend_from_slice(&[0, 0, 0]); // bufferSizeDB
    decoder_config.extend_from_slice(&0u32.to_be_bytes()); // maxBitrate
    decoder_config.extend_from_slice(&0u32.to_be_bytes()); // avgBitrate
    decoder_config.extend_from_slice(&decoder_specific);
    let decoder_config = descriptor(0x04, &decoder_config);
    let sl_config = descriptor(0x06, &[0x02]);

    let mut es_descriptor = Vec::with_capacity(3 + decoder_config.len() + sl_config.len());
    es_descriptor.extend_from_slice(&0x0001u16.to_be_bytes()); // ES_ID
    es_descriptor.push(0); // flags/priority
    es_descriptor.extend_from_slice(&decoder_config);
    es_descriptor.extend_from_slice(&sl_config);
    let es_descriptor = descriptor(0x03, &es_descriptor);

    let mut esds = Vec::with_capacity(4 + es_descriptor.len());
    esds.extend_from_slice(&[0, 0, 0, 0]); // version & flags
    esds.extend_from_slice(&es_descriptor);
    esds
}

/// MPEG-4 descriptor: tag + 1/2/3/4-байтовая длина (7-bit continuation).
fn descriptor(tag: u8, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(2 + payload.len() + 3);
    out.push(tag);
    let len = payload.len();
    if len < 0x80 {
        out.push(len as u8);
    } else if len <= 0xFF {
        out.push(0x81);
        out.push(len as u8);
    } else if len <= 0xFFFF {
        out.push(0x82);
        out.push((len >> 8) as u8);
        out.push(len as u8);
    } else {
        out.push(0x83);
        out.push((len >> 16) as u8);
        out.push((len >> 8) as u8);
        out.push(len as u8);
    }
    out.extend_from_slice(payload);
    out
}

fn write_box(buf: &mut Vec<u8>, box_type: &[u8; 4], payload: &[u8]) -> Result<(), ContainerError> {
    let size = payload.len().checked_add(8).ok_or(ContainerError::TooLarge)?;
    let size = u32::try_from(size).map_err(|_| ContainerError::TooLarge)?;
    buf.reserve(size as usize);
    buf.extend_from_slice(&size.to_be_bytes());
    buf.extend_from_slice(box_type);
    buf.extend_from_slice(payload);
    Ok(())
}

fn video_frame_ticks(fps: u32) -> Result<u32, ContainerError> {
    if fps == 0 {
        return Err(ContainerError::InvalidConfig);
    }
    Ok((VIDEO_TIMESCALE / fps).max(1))
}

fn video_stts_deltas(samples: &[VideoSample], frame_ticks: u32) -> Vec<u32> {
    let mut deltas = Vec::with_capacity(samples.len());
    for pair in samples.windows(2) {
        deltas.push(pair[1].pts_ticks.saturating_sub(pair[0].pts_ticks).max(1));
    }
    // Последний сэмпл: длительность кадра по fps (а не копия предыдущей
    // дельты и не 0); `mdhd duration` всё равно равен сумме.
    deltas.push(frame_ticks.max(1));
    deltas
}

fn checked_sum_u32(deltas: &[u32]) -> Result<u32, ContainerError> {
    let sum: u64 = deltas.iter().map(|delta| u64::from(*delta)).sum();
    u32::try_from(sum).map_err(|_| ContainerError::TooLarge)
}

fn ticks_to_ms(ticks: u32, timescale: u32) -> u32 {
    if timescale == 0 {
        return 0;
    }
    let ms = (u64::from(ticks) * u64::from(MOVIE_TIMESCALE) + u64::from(timescale) / 2) / u64::from(timescale);
    u32::try_from(ms).unwrap_or(u32::MAX)
}

fn pts_us_to_ticks(pts_us: u64) -> Result<u32, ContainerError> {
    let ticks = (u128::from(pts_us) * u128::from(VIDEO_TIMESCALE) + 500_000) / 1_000_000;
    u32::try_from(ticks).map_err(|_| ContainerError::TooLarge)
}

/// Конвертирует Annex-B сэмпл в length-prefixed; уже length-prefixed вход
/// возвращается как есть (идемпотентность). Также отдаёт NAL'ы для avcC.
fn normalize_video_sample(data: &[u8]) -> Result<(Vec<u8>, Vec<Vec<u8>>), ContainerError> {
    if data.is_empty() {
        return Err(ContainerError::EmptySample);
    }
    if let Some(nals) = parse_length_prefixed(data) {
        let owned: Vec<Vec<u8>> = nals.into_iter().map(<[u8]>::to_vec).collect();
        let mut out = Vec::with_capacity(data.len());
        for nal in &owned {
            push_length_prefixed(&mut out, nal)?;
        }
        return Ok((out, owned));
    }

    let nals: Vec<Vec<u8>> = annexb_nals(data).into_iter().map(<[u8]>::to_vec).collect();
    if nals.is_empty() {
        return Err(ContainerError::EmptySample);
    }
    let mut out = Vec::with_capacity(data.len());
    for nal in &nals {
        push_length_prefixed(&mut out, nal)?;
    }
    Ok((out, nals))
}

fn push_length_prefixed(out: &mut Vec<u8>, nal: &[u8]) -> Result<(), ContainerError> {
    let len = u32::try_from(nal.len()).map_err(|_| ContainerError::TooLarge)?;
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(nal);
    Ok(())
}

/// Разбирает уже length-prefixed NAL'ы; `None`, если формат не сходится.
fn parse_length_prefixed(data: &[u8]) -> Option<Vec<&[u8]>> {
    if data.len() < 5 {
        return None;
    }
    let mut nals = Vec::new();
    let mut cursor = 0usize;
    while cursor < data.len() {
        let header_end = cursor.checked_add(4)?;
        if header_end > data.len() {
            return None;
        }
        let len = u32::from_be_bytes(data[cursor..header_end].try_into().ok()?) as usize;
        if len == 0 {
            return None;
        }
        let nal_end = header_end.checked_add(len)?;
        if nal_end > data.len() {
            return None;
        }
        let nal = &data[header_end..nal_end];
        // forbidden_zero_bit обязан быть 0, иначе это не H.264 NAL.
        if nal[0] & 0x80 != 0 {
            return None;
        }
        nals.push(nal);
        cursor = nal_end;
    }
    if nals.is_empty() {
        None
    } else {
        Some(nals)
    }
}

/// Режет Annex-B на NAL без start code; хвостовые нули (trailing_zero_8bits)
/// снимаются.
fn annexb_nals(data: &[u8]) -> Vec<&[u8]> {
    vks_common::annexb::split_annexb(data, false)
        .map(trim_trailing_zeros)
        .filter(|nal| !nal.is_empty())
        .collect()
}

fn parameter_sets_from_nals(nals: &[Vec<u8>]) -> (Option<Vec<u8>>, Option<Vec<u8>>) {
    let mut sps = None;
    let mut pps = None;
    for nal in nals {
        let Some(&header) = nal.first() else { continue };
        match header & 0x1f {
            7 if sps.is_none() => sps = Some(nal.clone()),
            8 if pps.is_none() => pps = Some(nal.clone()),
            _ => {}
        }
    }
    (sps, pps)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPS: &[u8] = &[0x67, 0x42, 0xc0, 0x1f, 0x00, 0x00, 0x03, 0x00, 0x04];
    const PPS: &[u8] = &[0x68, 0xce, 0x3c, 0x80];

    fn video_config() -> ContainerConfig {
        ContainerConfig {
            width: 640,
            height: 360,
            fps: 30,
            video_codec: VideoCodec::H264,
            audio: None,
            max_size_bytes: 5 * 1024 * 1024,
        }
    }

    fn with_start_code(nal: &[u8]) -> Vec<u8> {
        let mut out = vec![0, 0, 0, 1];
        out.extend_from_slice(nal);
        out
    }

    fn access_unit(idr: bool, id: u8) -> Vec<u8> {
        let mut out = with_start_code(SPS);
        out.extend_from_slice(&with_start_code(PPS));
        let nal = if idr { vec![0x65, 0x88, 0x80 | (id & 0x3f)] } else { vec![0x41, 0x9a, id] };
        out.extend_from_slice(&with_start_code(&nal));
        out
    }

    fn three_frame_writer() -> PlayableContainerWriter {
        let mut writer = PlayableContainerWriter::new(video_config());
        writer.write_video_sample(&access_unit(true, 1), 0, true).unwrap();
        writer.write_video_sample(&access_unit(false, 2), 33_333, false).unwrap();
        writer.write_video_sample(&access_unit(false, 3), 66_666, false).unwrap();
        writer
    }

    #[derive(Debug, Clone, Copy)]
    struct BoxRef<'a> {
        kind: [u8; 4],
        start: usize,
        payload_start: usize,
        payload: &'a [u8],
    }

    fn parse_boxes(data: &[u8]) -> Vec<BoxRef<'_>> {
        let mut boxes = Vec::new();
        let mut cursor = 0usize;
        while cursor + 8 <= data.len() {
            let size = u32::from_be_bytes(data[cursor..cursor + 4].try_into().unwrap()) as usize;
            if size < 8 || cursor + size > data.len() {
                break;
            }
            let mut kind = [0u8; 4];
            kind.copy_from_slice(&data[cursor + 4..cursor + 8]);
            boxes.push(BoxRef {
                kind,
                start: cursor,
                payload_start: cursor + 8,
                payload: &data[cursor + 8..cursor + size],
            });
            cursor += size;
        }
        boxes
    }

    fn find_box<'a>(data: &'a [u8], kind: &[u8; 4]) -> Option<BoxRef<'a>> {
        parse_boxes(data).into_iter().find(|entry| &entry.kind == kind)
    }

    fn find_box_after<'a>(data: &'a [u8], skip: usize, kind: &[u8; 4]) -> Option<BoxRef<'a>> {
        find_box(data.get(skip..)?, kind)
    }

    fn u32_at(data: &[u8], offset: usize) -> u32 {
        u32::from_be_bytes(data[offset..offset + 4].try_into().unwrap())
    }

    fn traks(bytes: &[u8]) -> Vec<BoxRef<'_>> {
        let moov = find_box(bytes, b"moov").expect("moov");
        parse_boxes(moov.payload).into_iter().filter(|entry| &entry.kind == b"trak").collect()
    }

    fn video_trak(bytes: &[u8]) -> BoxRef<'_> {
        traks(bytes)
            .into_iter()
            .find(|trak| {
                find_box(trak.payload, b"mdia")
                    .and_then(|mdia| find_box(mdia.payload, b"hdlr"))
                    .is_some_and(|hdlr| &hdlr.payload[8..12] == b"vide")
            })
            .expect("video trak")
    }

    fn audio_trak(bytes: &[u8]) -> Option<BoxRef<'_>> {
        traks(bytes).into_iter().find(|trak| {
            find_box(trak.payload, b"mdia")
                .and_then(|mdia| find_box(mdia.payload, b"hdlr"))
                .is_some_and(|hdlr| &hdlr.payload[8..12] == b"soun")
        })
    }

    fn stbl<'a>(trak: &'a BoxRef<'a>) -> BoxRef<'a> {
        let mdia = find_box(trak.payload, b"mdia").expect("mdia");
        let minf = find_box(mdia.payload, b"minf").expect("minf");
        find_box(minf.payload, b"stbl").expect("stbl")
    }

    fn stsd_entry<'a>(stbl: &'a BoxRef<'a>) -> BoxRef<'a> {
        let stsd = find_box(stbl.payload, b"stsd").expect("stsd");
        parse_boxes(&stsd.payload[8..]).into_iter().next().expect("sample entry")
    }

    fn mdhd<'a>(trak: &'a BoxRef<'a>) -> BoxRef<'a> {
        let mdia = find_box(trak.payload, b"mdia").expect("mdia");
        find_box(mdia.payload, b"mdhd").expect("mdhd")
    }

    fn stts_deltas(stbl: &BoxRef<'_>) -> Vec<u32> {
        let stts = find_box(stbl.payload, b"stts").expect("stts");
        let count = u32_at(stts.payload, 4) as usize;
        let mut deltas = Vec::new();
        for index in 0..count {
            let base = 8 + index * 8;
            let repeats = u32_at(stts.payload, base);
            let delta = u32_at(stts.payload, base + 4);
            for _ in 0..repeats {
                deltas.push(delta);
            }
        }
        deltas
    }

    fn stco_offsets(stbl: &BoxRef<'_>) -> Vec<u32> {
        let stco = find_box(stbl.payload, b"stco").expect("stco");
        let count = u32_at(stco.payload, 4) as usize;
        (0..count).map(|index| u32_at(stco.payload, 8 + index * 4)).collect()
    }

    fn stsz_sizes(stbl: &BoxRef<'_>) -> Vec<u32> {
        let stsz = find_box(stbl.payload, b"stsz").expect("stsz");
        let count = u32_at(stsz.payload, 8) as usize;
        (0..count).map(|index| u32_at(stsz.payload, 12 + index * 4)).collect()
    }

    fn stss_entries(stbl: &BoxRef<'_>) -> Vec<u32> {
        let stss = find_box(stbl.payload, b"stss").expect("stss");
        let count = u32_at(stss.payload, 4) as usize;
        (0..count).map(|index| u32_at(stss.payload, 8 + index * 4)).collect()
    }

    #[test]
    fn avcc_matches_sps_pps_and_samples_are_length_prefixed() {
        let bytes = three_frame_writer().finalize().expect("finalize");
        let trak = video_trak(&bytes);
        let table = stbl(&trak);
        let entry = stsd_entry(&table);
        assert_eq!(&entry.kind, b"avc1");
        let avcc = find_box_after(entry.payload, 78, b"avcC").expect("avcC child of avc1");
        assert_eq!(avcc.payload[0], 1, "configurationVersion");
        assert_eq!(avcc.payload[1], SPS[1], "AVCProfileIndication from SPS");
        assert_eq!(avcc.payload[2], SPS[2], "profile_compatibility from SPS");
        assert_eq!(avcc.payload[3], SPS[3], "AVCLevelIndication from SPS");
        assert_eq!(avcc.payload[4], 0xFF, "reserved + lengthSizeMinusOne");
        assert_eq!(avcc.payload[4] & 0x03, 3, "nal_length_size == 4");
        assert_eq!(avcc.payload[5] & 0x1f, 1, "one SPS");
        let sps_len = u16::from_be_bytes([avcc.payload[6], avcc.payload[7]]) as usize;
        assert_eq!(sps_len, SPS.len());
        assert_eq!(&avcc.payload[8..8 + sps_len], SPS, "SPS bytes preserved (emulation prevention kept)");
        let pps_base = 8 + sps_len;
        assert_eq!(avcc.payload[pps_base], 1, "one PPS");
        let pps_len = u16::from_be_bytes([avcc.payload[pps_base + 1], avcc.payload[pps_base + 2]]) as usize;
        assert_eq!(&avcc.payload[pps_base + 3..pps_base + 3 + pps_len], PPS);

        let first_offset = stco_offsets(&table)[0] as usize;
        let first_len = u32_at(&bytes, first_offset) as usize;
        assert_eq!(first_len, SPS.len(), "first NAL is SPS with 4-byte length prefix");
        assert_ne!(&bytes[first_offset..first_offset + 4], &[0, 0, 0, 1], "no Annex-B start code in mdat");
        assert_eq!(&bytes[first_offset + 4..first_offset + 4 + first_len], SPS);
    }

    #[test]
    fn annexb_3_and_4_byte_start_codes_are_converted() {
        let mut sample = vec![0, 0, 1];
        sample.extend_from_slice(SPS);
        sample.extend_from_slice(&with_start_code(PPS));
        sample.extend_from_slice(&with_start_code(&[0x65, 0x88, 0x84, 0x00, 0x00]));
        sample.extend_from_slice(&[0, 0]);
        let mut writer = PlayableContainerWriter::new(video_config());
        writer.write_video_sample(&sample, 0, true).expect("write");
        let bytes = writer.finalize().expect("finalize");
        let trak = video_trak(&bytes);
        let sizes = stsz_sizes(&stbl(&trak));
        let expected = 4 + SPS.len() + 4 + PPS.len() + 4 + 3;
        assert_eq!(sizes, vec![expected as u32], "trailing zero bytes trimmed");
        let offset = stco_offsets(&stbl(&trak))[0] as usize;
        let mut cursor = offset;
        for nal in [SPS, PPS, &[0x65, 0x88, 0x84][..]] {
            assert_eq!(u32_at(&bytes, cursor) as usize, nal.len());
            assert_eq!(&bytes[cursor + 4..cursor + 4 + nal.len()], nal);
            cursor += 4 + nal.len();
        }
    }

    #[test]
    fn already_length_prefixed_input_is_idempotent() {
        let mut prefixed = Vec::new();
        push_length_prefixed(&mut prefixed, SPS).unwrap();
        push_length_prefixed(&mut prefixed, PPS).unwrap();
        push_length_prefixed(&mut prefixed, &[0x65, 0x88]).unwrap();
        let mut writer = PlayableContainerWriter::new(video_config());
        writer.write_video_sample(&prefixed, 0, true).expect("write");
        let bytes = writer.finalize().expect("finalize");
        let offset = stco_offsets(&stbl(&video_trak(&bytes)))[0] as usize;
        assert_eq!(&bytes[offset..offset + prefixed.len()], prefixed.as_slice());
    }

    #[test]
    fn emulation_prevention_sps_is_preserved_in_avcc() {
        let sps = [0x67, 0x4d, 0x40, 0x1f, 0x00, 0x00, 0x03, 0x00, 0x04];
        let sample = [with_start_code(&sps), with_start_code(PPS), with_start_code(&[0x65, 0x88])].concat();
        let mut writer = PlayableContainerWriter::new(video_config());
        writer.write_video_sample(&sample, 0, true).unwrap();
        let bytes = writer.finalize().unwrap();
        let trak = video_trak(&bytes);
        let table = stbl(&trak);
        let entry = stsd_entry(&table);
        let avcc = find_box_after(entry.payload, 78, b"avcC").unwrap();
        let sps_len = u16::from_be_bytes([avcc.payload[6], avcc.payload[7]]) as usize;
        assert_eq!(&avcc.payload[8..8 + sps_len], sps, "00 00 03 stays as in the bitstream");
    }

    #[test]
    fn high_profile_avcc_has_extension_bytes() {
        let sps = [0x67, 0x64, 0x00, 0x1f, 0x00, 0x00, 0x03, 0x00, 0x04];
        let sample = [with_start_code(&sps), with_start_code(PPS), with_start_code(&[0x65, 0x88])].concat();
        let mut writer = PlayableContainerWriter::new(video_config());
        writer.write_video_sample(&sample, 0, true).unwrap();
        let bytes = writer.finalize().unwrap();
        let trak = video_trak(&bytes);
        let table = stbl(&trak);
        let entry = stsd_entry(&table);
        let avcc = find_box_after(entry.payload, 78, b"avcC").unwrap();
        assert_eq!(&avcc.payload[avcc.payload.len() - 4..], &[0xFD, 0xF8, 0xF8, 0x00]);
    }

    #[test]
    fn stco_points_to_samples_and_stsz_sums_to_mdat_payload() {
        let bytes = three_frame_writer().finalize().unwrap();
        let trak = video_trak(&bytes);
        let table = stbl(&trak);
        let offsets = stco_offsets(&table);
        let sizes = stsz_sizes(&table);
        let mdat = find_box(&bytes, b"mdat").unwrap();
        assert_eq!(offsets.len(), sizes.len());
        assert_eq!(offsets[0] as usize, mdat.payload_start);
        for (index, offset) in offsets.iter().enumerate() {
            let expected =
                if index == 0 { mdat.payload_start } else { offsets[index - 1] as usize + sizes[index - 1] as usize };
            assert_eq!(*offset as usize, expected, "chunk {index} is contiguous");
        }
        let sum: u64 = sizes.iter().map(|size| u64::from(*size)).sum();
        assert_eq!(sum as usize, mdat.payload.len());
    }

    #[test]
    fn stts_sum_matches_mdhd_duration_and_last_delta_is_nonzero() {
        let bytes = three_frame_writer().finalize().unwrap();
        let trak = video_trak(&bytes);
        let mdhd = mdhd(&trak);
        assert_eq!(u32_at(mdhd.payload, 12), VIDEO_TIMESCALE, "video timescale 90000");
        let deltas = stts_deltas(&stbl(&trak));
        assert_eq!(deltas, vec![3_000, 3_000, 3_000]);
        let sum: u32 = deltas.iter().sum();
        assert_eq!(u32_at(mdhd.payload, 16), sum, "mdhd duration == sum(stts)");
    }

    #[test]
    fn faststart_layout_puts_moov_before_mdat_and_offsets_are_absolute() {
        let bytes = three_frame_writer().finalize().unwrap();
        let top = parse_boxes(&bytes);
        let kinds: Vec<[u8; 4]> = top.iter().map(|entry| entry.kind).collect();
        assert_eq!(&kinds[..3], &[*b"ftyp", *b"moov", *b"mdat"]);
        let ftyp = &top[0];
        let moov = &top[1];
        let mdat = &top[2];
        assert_eq!(moov.start, ftyp.payload_start + ftyp.payload.len());
        assert_eq!(mdat.start, moov.payload_start + moov.payload.len(), "mdat follows moov");
        assert_eq!(stco_offsets(&stbl(&video_trak(&bytes)))[0] as usize, mdat.payload_start);
    }

    #[test]
    fn oversized_offsets_fail_closed_without_allocation() {
        assert_eq!(build_stco(&[usize::MAX]).unwrap_err(), ContainerError::TooLarge);
        assert_eq!(pts_us_to_ticks(u64::MAX).unwrap_err(), ContainerError::TooLarge);
        assert_eq!(build_stsz(&[usize::MAX]).unwrap_err(), ContainerError::TooLarge);
    }

    #[test]
    fn empty_container_returns_error() {
        let mut writer = PlayableContainerWriter::new(video_config());
        assert_eq!(writer.finalize().unwrap_err(), ContainerError::Empty);
        assert_eq!(writer.finalize().unwrap_err(), ContainerError::AlreadyFinalized);
    }

    #[test]
    fn first_sample_must_be_keyframe_and_stss_has_no_fake_entry() {
        let mut writer = PlayableContainerWriter::new(video_config());
        assert_eq!(
            writer.write_video_sample(&access_unit(false, 1), 0, false).unwrap_err(),
            ContainerError::FirstSampleNotKeyframe
        );
        assert_eq!(writer.video_sample_count(), 0);

        let mut writer = PlayableContainerWriter::new(video_config());
        writer.write_video_sample(&access_unit(true, 1), 0, true).unwrap();
        let bytes = writer.finalize().unwrap();
        assert_eq!(stss_entries(&stbl(&video_trak(&bytes))), vec![1]);
    }

    #[test]
    fn missing_parameter_sets_are_an_error() {
        let mut writer = PlayableContainerWriter::new(video_config());
        writer.write_video_sample(&with_start_code(&[0x65, 0x88]), 0, true).unwrap();
        assert_eq!(writer.finalize().unwrap_err(), ContainerError::MissingParameterSets);
    }

    #[test]
    fn parameter_set_change_mid_recording_is_rejected() {
        let mut writer = PlayableContainerWriter::new(video_config());
        writer.write_video_sample(&access_unit(true, 1), 0, true).unwrap();
        let changed_sps = [0x67, 0x42, 0xc0, 0x20, 0x00, 0x00, 0x03, 0x00, 0x04];
        let sample = [with_start_code(&changed_sps), with_start_code(PPS), with_start_code(&[0x41, 0x9a])].concat();
        assert_eq!(
            writer.write_video_sample(&sample, 33_333, false).unwrap_err(),
            ContainerError::ParameterSetsChanged
        );
    }

    #[test]
    fn backwards_timestamp_is_rejected() {
        let mut writer = PlayableContainerWriter::new(video_config());
        writer.write_video_sample(&access_unit(true, 1), 100_000, true).unwrap();
        assert_eq!(
            writer.write_video_sample(&access_unit(false, 2), 50_000, false).unwrap_err(),
            ContainerError::InvalidTimestamp(50_000)
        );
    }

    #[test]
    fn h265_is_rejected_instead_of_written_as_avc1() {
        let config = ContainerConfig { video_codec: VideoCodec::H265, ..video_config() };
        let mut writer = PlayableContainerWriter::new(config);
        assert_eq!(
            writer.write_video_sample(&access_unit(true, 1), 0, true).unwrap_err(),
            ContainerError::UnsupportedCodec
        );
        assert_eq!(writer.finalize().unwrap_err(), ContainerError::UnsupportedCodec);
    }

    #[test]
    fn audio_samples_without_config_are_rejected_and_no_fake_track_is_written() {
        let mut writer = PlayableContainerWriter::new(video_config());
        assert_eq!(writer.write_audio_sample(&[0x21, 0x10], 0).unwrap_err(), ContainerError::UnsupportedAudio);
        writer.write_video_sample(&access_unit(true, 1), 0, true).unwrap();
        let bytes = writer.finalize().unwrap();
        assert!(audio_trak(&bytes).is_none(), "video-only file has no soun trak");
        assert!(!bytes.windows(4).any(|window| window == b"mp4a"), "no fake mp4a");
        let mvhd = find_box(&bytes, b"moov").and_then(|moov| find_box(moov.payload, b"mvhd")).unwrap();
        assert_eq!(u32_at(mvhd.payload, 96), 2, "next_track_ID == 2 for video-only");
    }

    #[test]
    fn esds_aac_lc_asc_roundtrip() {
        let audio = AudioConfig::aac_lc(48_000, 2, [0x11, 0x90]);
        let config = ContainerConfig { audio: Some(audio.clone()), ..video_config() };
        let mut writer = PlayableContainerWriter::new(config);
        writer.write_video_sample(&access_unit(true, 1), 0, true).unwrap();
        for (index, frame) in [[0x21u8, 0x10], [0x21, 0x11], [0x21, 0x12]].into_iter().enumerate() {
            writer.write_audio_sample(&frame, index as u64 * 21_333).unwrap();
        }
        let bytes = writer.finalize().unwrap();
        let trak = audio_trak(&bytes).expect("audio trak");
        let mdhd = mdhd(&trak);
        assert_eq!(u32_at(mdhd.payload, 12), 48_000, "audio mdhd timescale = sample_rate");
        assert_eq!(u32_at(mdhd.payload, 16), 3 * AAC_FRAME_SAMPLES, "duration = sum(AAC frames)");
        let table = stbl(&trak);
        assert_eq!(stts_deltas(&table), vec![AAC_FRAME_SAMPLES; 3]);
        let entry = stsd_entry(&table);
        assert_eq!(&entry.kind, b"mp4a");
        let channels = u16::from_be_bytes([entry.payload[16], entry.payload[17]]);
        assert_eq!(channels, 2);
        let sample_rate = u32_at(entry.payload, 24) >> 16;
        assert_eq!(sample_rate, 48_000);
        let esds = find_box_after(entry.payload, 28, b"esds").expect("esds child of mp4a");
        assert_eq!(&esds.payload[..4], &[0, 0, 0, 0], "esds version/flags");
        assert_eq!(esds.payload[4], 0x03, "ES_Descriptor tag");
        assert!(esds.payload.windows(3).any(|window| window == [0x04, 0x11, 0x40]));
        assert!(esds.payload.windows(4).any(|window| window == [0x05, 0x02, 0x11, 0x90]), "DecoderSpecificInfo = ASC");
        assert!(
            esds.payload.windows(3).any(|window| window == [0x04, 0x11, 0x40]),
            "DecoderConfig objectTypeIndication"
        );
        assert!(esds.payload.windows(3).any(|window| window == [0x06, 0x01, 0x02]), "SLConfig");
        let mdat = find_box(&bytes, b"mdat").unwrap();
        let offsets = stco_offsets(&table);
        let audio_start = *offsets.first().unwrap() as usize;
        assert!(audio_start >= mdat.payload_start);
        assert_eq!(&bytes[audio_start..audio_start + 2], &[0x21, 0x10], "raw AAC, no ADTS header");
        let tkhd = find_box(trak.payload, b"tkhd").unwrap();
        assert_eq!(u32_at(tkhd.payload, 20), 64, "3*1024/48000 = 64 ms");
    }

    #[test]
    fn configured_audio_without_samples_does_not_write_an_empty_track() {
        let config = ContainerConfig { audio: Some(AudioConfig::aac_lc(48_000, 2, [0x11, 0x90])), ..video_config() };
        let mut writer = PlayableContainerWriter::new(config);
        writer.write_video_sample(&access_unit(true, 1), 0, true).unwrap();
        let bytes = writer.finalize().unwrap();
        assert!(audio_trak(&bytes).is_none());
    }

    #[test]
    fn invalid_audio_config_is_rejected() {
        let config = ContainerConfig { audio: Some(AudioConfig::aac_lc(48_000, 2, [])), ..video_config() };
        let mut writer = PlayableContainerWriter::new(config);
        assert_eq!(writer.write_audio_sample(&[0x21], 0).unwrap_err(), ContainerError::InvalidConfig);
    }

    #[test]
    fn storage_full_checks_transformed_size() {
        let config = ContainerConfig { max_size_bytes: 1_000, ..video_config() };
        let mut writer = PlayableContainerWriter::new(config);
        let mut sample = with_start_code(&[0x65u8; 2_000]);
        sample.extend_from_slice(&[0, 0, 0, 1, 0x41]);
        let err = writer.write_video_sample(&sample, 0, true).unwrap_err();
        assert!(matches!(err, ContainerError::StorageFull { max: 1_000 }), "transformed size counts: {err:?}");
    }
}
