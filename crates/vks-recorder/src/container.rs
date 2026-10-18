//! Запись воспроизводимого медиаконтейнера: [`PlayableContainerWriter`], [`ContainerConfig`], [`ContainerError`].
//!
//! Обеспечивает запись валидного контейнера стандарта ISOBMFF (MP4) с заголовками
//! `ftyp`, `moov` (видео и аудио треки) и `mdat` для воспроизведения записанных сессий (M5.5).

use thiserror::Error;

use crate::recording::{Segment, SegmentKind};

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
#[allow(dead_code)]
struct SampleEntry {
    track_id: u32,
    timestamp_ms: u64,
    size: usize,
    is_keyframe: bool,
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
        self.samples.push(SampleEntry {
            track_id: 1, // Video track
            timestamp_ms,
            size: data.len(),
            is_keyframe,
        });
        self.media_data.extend_from_slice(data);
        self.duration_ms = self.duration_ms.max(timestamp_ms);
        Ok(())
    }

    /// Записывает аудиокадр/пакет в контейнер.
    pub fn write_audio_frame(&mut self, timestamp_ms: u64, data: &[u8]) -> Result<(), ContainerError> {
        self.check_capacity(data.len())?;
        self.samples.push(SampleEntry {
            track_id: 2, // Audio track
            timestamp_ms,
            size: data.len(),
            is_keyframe: true,
        });
        self.media_data.extend_from_slice(data);
        self.duration_ms = self.duration_ms.max(timestamp_ms);
        Ok(())
    }

    /// Записывает сегмент сессии записи в контейнер.
    pub fn write_segment(&mut self, segment: &Segment, sample_data: &[u8]) -> Result<(), ContainerError> {
        match segment.kind {
            SegmentKind::CompositeVideo | SegmentKind::TrackTrack => {
                self.write_video_frame(segment.duration_ms, true, sample_data)
            }
            SegmentKind::Audio => self.write_audio_frame(segment.duration_ms, sample_data),
        }
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

        let mut output = Vec::with_capacity(self.media_data.len() + 1024);

        // 1. Box 'ftyp' (File Type Box)
        let mut ftyp_payload = Vec::new();
        ftyp_payload.extend_from_slice(b"isom"); // Major brand
        ftyp_payload.extend_from_slice(&0x00000200u32.to_be_bytes()); // Minor version
        ftyp_payload.extend_from_slice(b"isom"); // Compatible brands
        ftyp_payload.extend_from_slice(b"iso2");
        ftyp_payload.extend_from_slice(b"mp41");
        write_box(&mut output, b"ftyp", &ftyp_payload);

        // 2. Box 'moov' (Movie Box with metadata)
        let moov_payload = self.build_moov();
        write_box(&mut output, b"moov", &moov_payload);

        // 3. Box 'mdat' (Media Data Box with actual sample payload)
        write_box(&mut output, b"mdat", &self.media_data);

        Ok(output)
    }

    fn check_capacity(&self, additional: usize) -> Result<(), ContainerError> {
        if self.finalized {
            return Err(ContainerError::AlreadyFinalized);
        }
        let total = self.media_data.len() + additional + 1024;
        if total > self.config.max_size_bytes {
            return Err(ContainerError::StorageFull { max: self.config.max_size_bytes });
        }
        Ok(())
    }

    fn build_moov(&self) -> Vec<u8> {
        let mut moov = Vec::new();

        // 2.1 'mvhd' (Movie Header)
        let mut mvhd = Vec::new();
        mvhd.extend_from_slice(&[0, 0, 0, 0]); // Version & flags
        mvhd.extend_from_slice(&0u32.to_be_bytes()); // Creation time
        mvhd.extend_from_slice(&0u32.to_be_bytes()); // Modification time
        mvhd.extend_from_slice(&1000u32.to_be_bytes()); // Timescale (1000 = ms)
        mvhd.extend_from_slice(&(self.duration_ms as u32).to_be_bytes()); // Duration
        mvhd.extend_from_slice(&0x00010000u32.to_be_bytes()); // Rate 1.0
        mvhd.extend_from_slice(&0x0100u16.to_be_bytes()); // Volume 1.0
        mvhd.extend_from_slice(&[0u8; 10]); // Reserved
                                            // Unity matrix
        let matrix: [u32; 9] = [0x00010000, 0, 0, 0, 0x00010000, 0, 0, 0, 0x40000000];
        for val in matrix {
            mvhd.extend_from_slice(&val.to_be_bytes());
        }
        mvhd.extend_from_slice(&[0u8; 24]); // Pre-defined
        mvhd.extend_from_slice(&3u32.to_be_bytes()); // Next track ID
        write_box(&mut moov, b"mvhd", &mvhd);

        // 2.2 'trak' Video (Track ID 1)
        let video_trak = self.build_video_trak();
        write_box(&mut moov, b"trak", &video_trak);

        // 2.3 'trak' Audio (Track ID 2)
        let audio_trak = self.build_audio_trak();
        write_box(&mut moov, b"trak", &audio_trak);

        moov
    }

    fn build_video_trak(&self) -> Vec<u8> {
        let mut trak = Vec::new();

        // tkhd
        let mut tkhd = Vec::new();
        tkhd.extend_from_slice(&[0, 0, 0, 0x07]); // Enabled, in movie, in preview
        tkhd.extend_from_slice(&0u32.to_be_bytes());
        tkhd.extend_from_slice(&0u32.to_be_bytes());
        tkhd.extend_from_slice(&1u32.to_be_bytes()); // Track ID 1
        tkhd.extend_from_slice(&0u32.to_be_bytes());
        tkhd.extend_from_slice(&(self.duration_ms as u32).to_be_bytes());
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
        mdhd.extend_from_slice(&1000u32.to_be_bytes()); // Timescale
        mdhd.extend_from_slice(&(self.duration_ms as u32).to_be_bytes());
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

        write_box(&mut minf, b"stbl", &stbl);
        write_box(&mut mdia, b"minf", &minf);
        write_box(&mut trak, b"mdia", &mdia);

        trak
    }

    fn build_audio_trak(&self) -> Vec<u8> {
        let mut trak = Vec::new();

        // tkhd
        let mut tkhd = Vec::new();
        tkhd.extend_from_slice(&[0, 0, 0, 0x07]);
        tkhd.extend_from_slice(&0u32.to_be_bytes());
        tkhd.extend_from_slice(&0u32.to_be_bytes());
        tkhd.extend_from_slice(&2u32.to_be_bytes()); // Track ID 2
        tkhd.extend_from_slice(&0u32.to_be_bytes());
        tkhd.extend_from_slice(&(self.duration_ms as u32).to_be_bytes());
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
        mdhd.extend_from_slice(&(self.duration_ms as u32).to_be_bytes());
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

        write_box(&mut minf, b"stbl", &stbl);
        write_box(&mut mdia, b"minf", &minf);
        write_box(&mut trak, b"mdia", &mdia);

        trak
    }
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
