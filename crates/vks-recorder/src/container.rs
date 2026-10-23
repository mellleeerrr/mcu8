//! Запись воспроизводимого медиаконтейнера: [`PlayableContainerWriter`], [`ContainerConfig`], [`ContainerError`].
//!
//! Обеспечивает запись валидного контейнера стандарта ISOBMFF (MP4) с заголовками
//! `ftyp`, `moov` (видео и аудио треки со всеми обязательными таблицами `stbl`: `stsd`, `stts`, `stss`, `stsc`, `stsz`, `stco`)
//! и `mdat` для воспроизведения записанных сессий (M5.5).

use thiserror::Error;

/// Ошибки записи медиаконтейнера.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ContainerError {
    /// Достигнут лимит свободного места / квоты хранилища (Storage full).
    #[error("хранилище переполнено: превышен лимит {max} байт")]
    StorageFull { max: usize },
    /// Контейнер уже финализирован.
    #[error("контейнер уже финализирован")]
    AlreadyFinalized,
    /// Некорректная временная метка.
    #[error("некорректный timestamp {0} ms")]
    InvalidTimestamp(u64),
}

/// Конфигурация медиаконтейнера.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContainerConfig {
    /// Ширина видео в пикселях.
    pub width: u32,
    /// Высота видео в пикселях.
    pub height: u32,
    /// Кадров в секунду (fps).
    pub fps: u32,
    /// Частота дискретизации аудио (например 48000 Гц).
    pub sample_rate: u32,
    /// Число аудиоканалов (1 = моно, 2 = стерео).
    pub channels: u16,
    /// Максимальный размер контейнера в байтах (квота).
    pub max_size_bytes: usize,
}

impl Default for ContainerConfig {
    fn default() -> Self {
        Self {
            width: 1280,
            height: 720,
            fps: 30,
            sample_rate: 48000,
            channels: 2,
            max_size_bytes: 50 * 1024 * 1024, // 50 MB default quota
        }
    }
}

/// Запись кадра или сэмпла в контейнере.
#[derive(Debug, Clone)]
pub struct SampleEntry {
    /// Идентификатор трека (1 = видео, 2 = аудио).
    pub track_id: u32,
    /// Временная метка в мс.
    pub timestamp_ms: u64,
    /// Размер сэмпла в байтах.
    pub size: usize,
    /// Смещение сэмпла внутри media_data.
    pub offset: usize,
    /// Является ли ключевым кадром (для видео).
    pub is_keyframe: bool,
}

/// Записыватель воспроизводимого ISOBMFF (MP4) контейнера.
pub struct PlayableContainerWriter {
    config: ContainerConfig,
    samples: Vec<SampleEntry>,
    media_data: Vec<u8>,
    duration_ms: u64,
    finalized: bool,
}

impl PlayableContainerWriter {
    /// Создаёт новый записыватель контейнера с заданной конфигурацией.
    pub fn new(config: ContainerConfig) -> Self {
        Self { config, samples: Vec::new(), media_data: Vec::new(), duration_ms: 0, finalized: false }
    }

    /// Записывает видеокадр в контейнер.
    pub fn write_video_frame(
        &mut self,
        timestamp_ms: u64,
        is_keyframe: bool,
        data: &[u8],
    ) -> Result<(), ContainerError> {
        self.check_capacity(data.len())?;
        let offset = self.media_data.len();
        self.samples.push(SampleEntry {
            track_id: 1, // Video track
            timestamp_ms,
            size: data.len(),
            offset,
            is_keyframe,
        });
        self.media_data.extend_from_slice(data);
        self.duration_ms = self.duration_ms.max(timestamp_ms);
        Ok(())
    }

    /// Записывает аудиокадр/пакет в контейнер.
    pub fn write_audio_frame(&mut self, timestamp_ms: u64, data: &[u8]) -> Result<(), ContainerError> {
        self.check_capacity(data.len())?;
        let offset = self.media_data.len();
        self.samples.push(SampleEntry {
            track_id: 2, // Audio track
            timestamp_ms,
            size: data.len(),
            offset,
            is_keyframe: true,
        });
        self.media_data.extend_from_slice(data);
        self.duration_ms = self.duration_ms.max(timestamp_ms);
        Ok(())
    }

    /// Записывает видео-сэмпл (алиас для write_video_frame).
    pub fn write_video_sample(
        &mut self,
        data: &[u8],
        duration_or_ts_ms: u64,
        is_keyframe: bool,
    ) -> Result<(), ContainerError> {
        self.write_video_frame(duration_or_ts_ms, is_keyframe, data)
    }

    /// Записывает аудио-сэмпл (алиас для write_audio_frame).
    pub fn write_audio_sample(&mut self, data: &[u8], duration_or_ts_ms: u64) -> Result<(), ContainerError> {
        self.write_audio_frame(duration_or_ts_ms, data)
    }

    /// Число записанных сэмплов в контейнере.
    pub fn sample_count(&self) -> usize {
        self.samples.len()
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

        // 1. Box 'ftyp' (File Type Box)
        let ftyp_box = self.build_ftyp_box();
        let ftyp_len = ftyp_box.len();

        let video_sample_count = self.samples.iter().filter(|s| s.track_id == 1).count();
        let audio_sample_count = self.samples.iter().filter(|s| s.track_id == 2).count();

        // Pass 1: Вычисляем детерминированный размер 'moov' с фиктивными смещениями
        let dummy_video_offsets = vec![0usize; video_sample_count];
        let dummy_audio_offsets = vec![0usize; audio_sample_count];
        let dummy_moov = self.build_moov(&dummy_video_offsets, &dummy_audio_offsets);
        let moov_len = dummy_moov.len() + 8; // + 8 байт заголовок write_box

        // 2. Начало полезной нагрузки mdat в результирующем файле
        let mdat_payload_offset = ftyp_len + moov_len + 8; // 8 байт заголовок mdat

        // Pass 2: Рассчитываем точные смещения чанков в файле
        let real_video_offsets: Vec<usize> =
            self.samples.iter().filter(|s| s.track_id == 1).map(|s| mdat_payload_offset + s.offset).collect();

        let real_audio_offsets: Vec<usize> =
            self.samples.iter().filter(|s| s.track_id == 2).map(|s| mdat_payload_offset + s.offset).collect();

        let mut output = Vec::with_capacity(mdat_payload_offset + self.media_data.len());
        output.extend_from_slice(&ftyp_box);

        let moov_payload = self.build_moov(&real_video_offsets, &real_audio_offsets);
        write_box(&mut output, b"moov", &moov_payload);

        // 3. Box 'mdat' (Media Data Box with actual sample payload)
        write_box(&mut output, b"mdat", &self.media_data);

        Ok(output)
    }

    fn check_capacity(&self, additional: usize) -> Result<(), ContainerError> {
        if self.finalized {
            return Err(ContainerError::AlreadyFinalized);
        }
        let total = self.media_data.len() + additional + 2048;
        if total > self.config.max_size_bytes {
            return Err(ContainerError::StorageFull { max: self.config.max_size_bytes });
        }
        Ok(())
    }

    fn build_ftyp_box(&self) -> Vec<u8> {
        let mut ftyp_payload = Vec::new();
        ftyp_payload.extend_from_slice(b"isom"); // Major brand
        ftyp_payload.extend_from_slice(&0x00000200u32.to_be_bytes()); // Minor version
        ftyp_payload.extend_from_slice(b"isom"); // Compatible brands
        ftyp_payload.extend_from_slice(b"iso2");
        ftyp_payload.extend_from_slice(b"mp41");
        let mut box_buf = Vec::new();
        write_box(&mut box_buf, b"ftyp", &ftyp_payload);
        box_buf
    }

    fn effective_duration(&self) -> u32 {
        let default_frame_ms = (1000 / self.config.fps).max(1) as u64;
        self.duration_ms.max(default_frame_ms) as u32
    }

    fn build_moov(&self, video_offsets: &[usize], audio_offsets: &[usize]) -> Vec<u8> {
        let mut moov = Vec::new();
        let dur = self.effective_duration();

        // 2.1 'mvhd' (Movie Header)
        let mut mvhd = Vec::new();
        mvhd.extend_from_slice(&[0, 0, 0, 0]); // Version & flags
        mvhd.extend_from_slice(&0u32.to_be_bytes()); // Creation time
        mvhd.extend_from_slice(&0u32.to_be_bytes()); // Modification time
        mvhd.extend_from_slice(&1000u32.to_be_bytes()); // Timescale (1000 = ms)
        mvhd.extend_from_slice(&dur.to_be_bytes()); // Duration
        mvhd.extend_from_slice(&0x00010000u32.to_be_bytes()); // Rate 1.0
        mvhd.extend_from_slice(&0x0100u16.to_be_bytes()); // Volume 1.0
        mvhd.extend_from_slice(&[0u8; 10]); // Reserved
        let matrix: [u32; 9] = [0x00010000, 0, 0, 0, 0x00010000, 0, 0, 0, 0x40000000];
        for val in matrix {
            mvhd.extend_from_slice(&val.to_be_bytes());
        }
        mvhd.extend_from_slice(&[0u8; 24]); // Pre-defined
        mvhd.extend_from_slice(&3u32.to_be_bytes()); // Next track ID
        write_box(&mut moov, b"mvhd", &mvhd);

        // 2.2 'trak' Video (Track ID 1)
        let video_trak = self.build_video_trak(video_offsets, dur);
        write_box(&mut moov, b"trak", &video_trak);

        // 2.3 'trak' Audio (Track ID 2)
        let audio_trak = self.build_audio_trak(audio_offsets, dur);
        write_box(&mut moov, b"trak", &audio_trak);

        moov
    }

    fn build_video_trak(&self, offsets: &[usize], dur: u32) -> Vec<u8> {
        let mut trak = Vec::new();

        // tkhd
        let mut tkhd = Vec::new();
        tkhd.extend_from_slice(&[0, 0, 0, 0x07]); // Enabled, in movie, in preview
        tkhd.extend_from_slice(&0u32.to_be_bytes());
        tkhd.extend_from_slice(&0u32.to_be_bytes());
        tkhd.extend_from_slice(&1u32.to_be_bytes()); // Track ID 1
        tkhd.extend_from_slice(&0u32.to_be_bytes());
        tkhd.extend_from_slice(&dur.to_be_bytes());
        tkhd.extend_from_slice(&[0u8; 8]);
        tkhd.extend_from_slice(&0u16.to_be_bytes()); // Layer
        tkhd.extend_from_slice(&0u16.to_be_bytes()); // Alternate group
        tkhd.extend_from_slice(&0u16.to_be_bytes()); // Volume
        tkhd.extend_from_slice(&0u16.to_be_bytes());
        let matrix: [u32; 9] = [0x00010000, 0, 0, 0, 0x00010000, 0, 0, 0, 0x40000000];
        for val in matrix {
            tkhd.extend_from_slice(&val.to_be_bytes());
        }
        tkhd.extend_from_slice(&(self.config.width << 16).to_be_bytes()); // Width 16.16 fixed
        tkhd.extend_from_slice(&(self.config.height << 16).to_be_bytes()); // Height 16.16 fixed
        write_box(&mut trak, b"tkhd", &tkhd);

        // mdia
        let mut mdia = Vec::new();
        // mdhd
        let mut mdhd = Vec::new();
        mdhd.extend_from_slice(&[0, 0, 0, 0]);
        mdhd.extend_from_slice(&0u32.to_be_bytes());
        mdhd.extend_from_slice(&0u32.to_be_bytes());
        mdhd.extend_from_slice(&1000u32.to_be_bytes()); // Timescale (ms)
        mdhd.extend_from_slice(&dur.to_be_bytes());
        mdhd.extend_from_slice(&0x55c4u16.to_be_bytes()); // Language 'und'
        mdhd.extend_from_slice(&0u16.to_be_bytes());
        write_box(&mut mdia, b"mdhd", &mdhd);

        // hdlr
        let mut hdlr = Vec::new();
        hdlr.extend_from_slice(&[0, 0, 0, 0]);
        hdlr.extend_from_slice(&0u32.to_be_bytes());
        hdlr.extend_from_slice(b"vide"); // Handler type
        hdlr.extend_from_slice(&[0u8; 12]);
        hdlr.extend_from_slice(b"VideoHandler\0");
        write_box(&mut mdia, b"hdlr", &hdlr);

        // minf
        let mut minf = Vec::new();
        // vmhd
        let mut vmhd = Vec::new();
        vmhd.extend_from_slice(&[0, 0, 0, 1]); // Flags = 1
        vmhd.extend_from_slice(&[0u8; 8]);
        write_box(&mut minf, b"vmhd", &vmhd);

        // dinf
        let mut dinf = Vec::new();
        let mut dref = Vec::new();
        dref.extend_from_slice(&[0, 0, 0, 0]);
        dref.extend_from_slice(&1u32.to_be_bytes()); // 1 entry
        let mut url_box = Vec::new();
        url_box.extend_from_slice(&[0, 0, 0, 1]); // Self-contained flag
        write_box(&mut dref, b"url ", &url_box);
        write_box(&mut dinf, b"dref", &dref);
        write_box(&mut minf, b"dinf", &dinf);

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
        avc1.extend_from_slice(&0x00480000u32.to_be_bytes()); // 72 dpi
        avc1.extend_from_slice(&0x00480000u32.to_be_bytes());
        avc1.extend_from_slice(&0u32.to_be_bytes());
        avc1.extend_from_slice(&1u16.to_be_bytes()); // Frame count
        avc1.extend_from_slice(&[0u8; 32]); // Compressor name
        avc1.extend_from_slice(&0x0018u16.to_be_bytes()); // Depth 24
        avc1.extend_from_slice(&(-1i16).to_be_bytes());
        write_box(&mut stsd, b"avc1", &avc1);
        write_box(&mut stbl, b"stsd", &stsd);

        let video_samples: Vec<&SampleEntry> = self.samples.iter().filter(|s| s.track_id == 1).collect();

        // stts (Time-to-sample)
        let default_delta = (1000 / self.config.fps).max(1);
        let mut deltas = Vec::new();
        for i in 0..video_samples.len() {
            let d = if i + 1 < video_samples.len() {
                (video_samples[i + 1].timestamp_ms.saturating_sub(video_samples[i].timestamp_ms)) as u32
            } else if i > 0 {
                (video_samples[i].timestamp_ms.saturating_sub(video_samples[i - 1].timestamp_ms)) as u32
            } else {
                default_delta
            };
            deltas.push(if d == 0 { default_delta } else { d });
        }
        let stts = build_stts(&deltas);
        write_box(&mut stbl, b"stts", &stts);

        // stss (Sync samples / keyframes)
        let mut keyframes = Vec::new();
        for (i, s) in video_samples.iter().enumerate() {
            if s.is_keyframe {
                keyframes.push((i + 1) as u32);
            }
        }
        if keyframes.is_empty() && !video_samples.is_empty() {
            keyframes.push(1);
        }
        let stss = build_stss(&keyframes);
        write_box(&mut stbl, b"stss", &stss);

        // stsc (Sample to chunk)
        let stsc = build_stsc(video_samples.len() as u32);
        write_box(&mut stbl, b"stsc", &stsc);

        // stsz (Sample sizes)
        let sizes: Vec<usize> = video_samples.iter().map(|s| s.size).collect();
        let stsz = build_stsz(&sizes);
        write_box(&mut stbl, b"stsz", &stsz);

        // stco (Chunk offsets)
        let stco = build_stco(offsets);
        write_box(&mut stbl, b"stco", &stco);

        write_box(&mut minf, b"stbl", &stbl);
        write_box(&mut mdia, b"minf", &minf);
        write_box(&mut trak, b"mdia", &mdia);

        trak
    }

    fn build_audio_trak(&self, offsets: &[usize], dur: u32) -> Vec<u8> {
        let mut trak = Vec::new();

        // tkhd
        let mut tkhd = Vec::new();
        tkhd.extend_from_slice(&[0, 0, 0, 0x07]);
        tkhd.extend_from_slice(&0u32.to_be_bytes());
        tkhd.extend_from_slice(&0u32.to_be_bytes());
        tkhd.extend_from_slice(&2u32.to_be_bytes()); // Track ID 2
        tkhd.extend_from_slice(&0u32.to_be_bytes());
        tkhd.extend_from_slice(&dur.to_be_bytes());
        tkhd.extend_from_slice(&[0u8; 8]);
        tkhd.extend_from_slice(&0u16.to_be_bytes());
        tkhd.extend_from_slice(&0u16.to_be_bytes());
        tkhd.extend_from_slice(&0x0100u16.to_be_bytes()); // Full audio volume
        tkhd.extend_from_slice(&0u16.to_be_bytes());
        let matrix: [u32; 9] = [0x00010000, 0, 0, 0, 0x00010000, 0, 0, 0, 0x40000000];
        for val in matrix {
            tkhd.extend_from_slice(&val.to_be_bytes());
        }
        tkhd.extend_from_slice(&0u32.to_be_bytes()); // Width 0 for audio
        tkhd.extend_from_slice(&0u32.to_be_bytes()); // Height 0
        write_box(&mut trak, b"tkhd", &tkhd);

        // mdia
        let mut mdia = Vec::new();
        let mut mdhd = Vec::new();
        mdhd.extend_from_slice(&[0, 0, 0, 0]);
        mdhd.extend_from_slice(&0u32.to_be_bytes());
        mdhd.extend_from_slice(&0u32.to_be_bytes());
        mdhd.extend_from_slice(&1000u32.to_be_bytes());
        mdhd.extend_from_slice(&dur.to_be_bytes());
        mdhd.extend_from_slice(&0x55c4u16.to_be_bytes());
        mdhd.extend_from_slice(&0u16.to_be_bytes());
        write_box(&mut mdia, b"mdhd", &mdhd);

        let mut hdlr = Vec::new();
        hdlr.extend_from_slice(&[0, 0, 0, 0]);
        hdlr.extend_from_slice(&0u32.to_be_bytes());
        hdlr.extend_from_slice(b"soun"); // Sound handler
        hdlr.extend_from_slice(&[0u8; 12]);
        hdlr.extend_from_slice(b"SoundHandler\0");
        write_box(&mut mdia, b"hdlr", &hdlr);

        let mut minf = Vec::new();
        let mut smhd = Vec::new();
        smhd.extend_from_slice(&[0, 0, 0, 0]);
        smhd.extend_from_slice(&0u16.to_be_bytes()); // Balance
        smhd.extend_from_slice(&0u16.to_be_bytes());
        write_box(&mut minf, b"smhd", &smhd);

        let mut dinf = Vec::new();
        let mut dref = Vec::new();
        dref.extend_from_slice(&[0, 0, 0, 0]);
        dref.extend_from_slice(&1u32.to_be_bytes());
        let mut url_box = Vec::new();
        url_box.extend_from_slice(&[0, 0, 0, 1]);
        write_box(&mut dref, b"url ", &url_box);
        write_box(&mut dinf, b"dref", &dref);
        write_box(&mut minf, b"dinf", &dinf);

        let mut stbl = Vec::new();
        let mut stsd = Vec::new();
        stsd.extend_from_slice(&[0, 0, 0, 0]);
        stsd.extend_from_slice(&1u32.to_be_bytes());
        let mut mp4a = Vec::new();
        mp4a.extend_from_slice(&[0u8; 6]);
        mp4a.extend_from_slice(&1u16.to_be_bytes());
        mp4a.extend_from_slice(&[0u8; 8]);
        mp4a.extend_from_slice(&self.config.channels.to_be_bytes());
        mp4a.extend_from_slice(&16u16.to_be_bytes()); // 16-bit
        mp4a.extend_from_slice(&0u32.to_be_bytes());
        mp4a.extend_from_slice(&(self.config.sample_rate << 16).to_be_bytes());
        write_box(&mut stsd, b"mp4a", &mp4a);
        write_box(&mut stbl, b"stsd", &stsd);

        let audio_samples: Vec<&SampleEntry> = self.samples.iter().filter(|s| s.track_id == 2).collect();

        // stts
        let mut deltas = Vec::new();
        for i in 0..audio_samples.len() {
            let d = if i + 1 < audio_samples.len() {
                (audio_samples[i + 1].timestamp_ms.saturating_sub(audio_samples[i].timestamp_ms)) as u32
            } else if i > 0 {
                (audio_samples[i].timestamp_ms.saturating_sub(audio_samples[i - 1].timestamp_ms)) as u32
            } else {
                20
            };
            deltas.push(if d == 0 { 20 } else { d });
        }
        let stts = build_stts(&deltas);
        write_box(&mut stbl, b"stts", &stts);

        // stsc
        let stsc = build_stsc(audio_samples.len() as u32);
        write_box(&mut stbl, b"stsc", &stsc);

        // stsz
        let sizes: Vec<usize> = audio_samples.iter().map(|s| s.size).collect();
        let stsz = build_stsz(&sizes);
        write_box(&mut stbl, b"stsz", &stsz);

        // stco
        let stco = build_stco(offsets);
        write_box(&mut stbl, b"stco", &stco);

        write_box(&mut minf, b"stbl", &stbl);
        write_box(&mut mdia, b"minf", &minf);
        write_box(&mut trak, b"mdia", &mdia);

        trak
    }
}

fn build_stts(deltas: &[u32]) -> Vec<u8> {
    let mut buf = Vec::new();
    buf.extend_from_slice(&[0, 0, 0, 0]); // version & flags
    if deltas.is_empty() {
        buf.extend_from_slice(&0u32.to_be_bytes());
    } else {
        let mut rle: Vec<(u32, u32)> = Vec::new();
        for &d in deltas {
            if let Some(last) = rle.last_mut() {
                if last.1 == d {
                    last.0 += 1;
                    continue;
                }
            }
            rle.push((1, d));
        }
        buf.extend_from_slice(&(rle.len() as u32).to_be_bytes());
        for (cnt, d) in rle {
            buf.extend_from_slice(&cnt.to_be_bytes());
            buf.extend_from_slice(&d.to_be_bytes());
        }
    }
    buf
}

fn build_stss(keyframe_indices_1based: &[u32]) -> Vec<u8> {
    let mut buf = Vec::new();
    buf.extend_from_slice(&[0, 0, 0, 0]);
    buf.extend_from_slice(&(keyframe_indices_1based.len() as u32).to_be_bytes());
    for &idx in keyframe_indices_1based {
        buf.extend_from_slice(&idx.to_be_bytes());
    }
    buf
}

fn build_stsc(sample_count: u32) -> Vec<u8> {
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
    buf
}

fn build_stsz(sizes: &[usize]) -> Vec<u8> {
    let mut buf = Vec::new();
    buf.extend_from_slice(&[0, 0, 0, 0]);
    buf.extend_from_slice(&0u32.to_be_bytes()); // variable sizes
    buf.extend_from_slice(&(sizes.len() as u32).to_be_bytes());
    for &s in sizes {
        buf.extend_from_slice(&(s as u32).to_be_bytes());
    }
    buf
}

fn build_stco(offsets: &[usize]) -> Vec<u8> {
    let mut buf = Vec::new();
    buf.extend_from_slice(&[0, 0, 0, 0]);
    buf.extend_from_slice(&(offsets.len() as u32).to_be_bytes());
    for &o in offsets {
        buf.extend_from_slice(&(o as u32).to_be_bytes());
    }
    buf
}

fn write_box(buf: &mut Vec<u8>, box_type: &[u8; 4], payload: &[u8]) {
    let size = (payload.len() + 8) as u32;
    buf.extend_from_slice(&size.to_be_bytes());
    buf.extend_from_slice(box_type);
    buf.extend_from_slice(payload);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_playable_container_writer_roundtrip() {
        let config = ContainerConfig {
            width: 1920,
            height: 1080,
            fps: 30,
            sample_rate: 48000,
            channels: 2,
            max_size_bytes: 10 * 1024 * 1024,
        };
        let mut writer = PlayableContainerWriter::new(config);

        writer.write_video_frame(0, true, &[0xAA; 512]).expect("write keyframe");
        writer.write_audio_frame(20, &[0xBB; 128]).expect("write audio");
        writer.write_video_frame(40, false, &[0xCC; 256]).expect("write p-frame");
        writer.write_audio_frame(40, &[0xDD; 128]).expect("write audio");

        assert_eq!(writer.current_size(), 512 + 128 + 256 + 128);

        let bytes = writer.finalize().expect("finalize container");
        assert!(bytes.len() > writer.current_size());

        // Проверяем наличие валидных ISO/IEC 14496-12 сигнатур
        assert_eq!(&bytes[4..8], b"ftyp");
        assert_eq!(&bytes[8..12], b"isom");

        // Проверяем наличие moov и mdat
        let moov_pos = bytes.windows(4).position(|w| w == b"moov").expect("moov present");
        let mdat_pos = bytes.windows(4).position(|w| w == b"mdat").expect("mdat present");
        assert!(moov_pos > 0);
        assert!(mdat_pos > moov_pos);

        // Проверяем наличие обязательных ISOBMFF таблиц внутри stbl
        assert!(bytes.windows(4).any(|w| w == b"stts"), "stts must be present");
        assert!(bytes.windows(4).any(|w| w == b"stss"), "stss must be present");
        assert!(bytes.windows(4).any(|w| w == b"stsc"), "stsc must be present");
        assert!(bytes.windows(4).any(|w| w == b"stsz"), "stsz must be present");
        assert!(bytes.windows(4).any(|w| w == b"stco"), "stco must be present");

        // Повторный finalize возвращает ошибку
        assert_eq!(writer.finalize().unwrap_err(), ContainerError::AlreadyFinalized);
    }

    #[test]
    fn test_playable_container_storage_full() {
        let config = ContainerConfig {
            max_size_bytes: 1000, // Small limit
            ..Default::default()
        };
        let mut writer = PlayableContainerWriter::new(config);
        let err = writer.write_video_frame(0, true, &[0u8; 2000]).unwrap_err();
        assert!(matches!(err, ContainerError::StorageFull { max: 1000 }));
    }
}
