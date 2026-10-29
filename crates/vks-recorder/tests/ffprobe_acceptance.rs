//! T09b acceptance: реальный x264 Annex-B и ADTS AAC прогоняются через
//! [`PlayableContainerWriter`], а результат проверяется **ffprobe/ffmpeg**.
//!
//! Это усиленный гейт: старый «ffprobe показывает кадры» проходил даже на
//! Annex-B в `mdat` без `avcC`. Здесь обязательны `is_avc=true`,
//! `nal_length_size=4`, `avcC` в `avc1`, time_base 1/90000, полное
//! декодирование, а также консистентность `stco`/`stsz`/`stts`.
//!
//! Тесты мягко пропускаются, если ffmpeg/ffprobe не установлены (как
//! остальные FFmpeg-тесты репозитория).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use vks_recorder::{AudioConfig, ContainerConfig, PlayableContainerWriter, VideoCodec};

const SPS: &[u8] = &[0x67, 0x42, 0xc0, 0x1f, 0x00, 0x00, 0x03, 0x00, 0x04];
const PPS: &[u8] = &[0x68, 0xce, 0x3c, 0x80];

fn ffmpeg_bin() -> Option<String> {
    let binary = std::env::var("VKS_FFMPEG_BIN").unwrap_or_else(|_| "ffmpeg".to_string());
    Command::new(&binary)
        .arg("-version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .ok()
        .filter(std::process::ExitStatus::success)
        .map(|_| binary)
}

fn ffprobe_bin() -> Option<String> {
    let binary = std::env::var("VKS_FFPROBE_BIN").unwrap_or_else(|_| "ffprobe".to_string());
    Command::new(&binary)
        .arg("-version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .ok()
        .filter(std::process::ExitStatus::success)
        .map(|_| binary)
}

fn temp_path(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("t09b_ffprobe");
    std::fs::create_dir_all(&dir).expect("create target tmp dir");
    dir.join(format!("{name}_{}", std::process::id()))
}

fn run_ffmpeg(ffmpeg: &str, args: &[&str]) -> Vec<u8> {
    let output = Command::new(ffmpeg).args(args).output().expect("spawn ffmpeg");
    assert!(output.status.success(), "ffmpeg failed: {}", String::from_utf8_lossy(&output.stderr));
    output.stdout
}

fn probe(ffprobe: &str, file: &Path, stream: &str, entries: &str) -> HashMap<String, String> {
    let output = Command::new(ffprobe)
        .args([
            "-v",
            "error",
            "-count_frames",
            "-select_streams",
            stream,
            "-show_entries",
            entries,
            "-of",
            "default=noprint_wrappers=1",
        ])
        .arg(file)
        .output()
        .expect("spawn ffprobe");
    assert!(output.status.success(), "ffprobe failed: {}", String::from_utf8_lossy(&output.stderr));
    let text = String::from_utf8_lossy(&output.stdout);
    let mut fields = HashMap::new();
    for line in text.lines() {
        if let Some((key, value)) = line.split_once('=') {
            fields.insert(key.trim().to_string(), value.trim().to_string());
        }
    }
    fields
}

fn decode_ok(ffmpeg: &str, file: &Path, map: &str) -> bool {
    Command::new(ffmpeg)
        .args(["-v", "error", "-i"])
        .arg(file)
        .args(["-map", map, "-f", "null", "-"])
        .output()
        .expect("spawn ffmpeg decode")
        .status
        .success()
}

#[derive(Debug, Clone, Copy)]
struct BoxRef<'a> {
    kind: [u8; 4],
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
        boxes.push(BoxRef { kind, payload_start: cursor + 8, payload: &data[cursor + 8..cursor + size] });
        cursor += size;
    }
    boxes
}

fn find_box<'a>(data: &'a [u8], kind: &[u8; 4]) -> Option<BoxRef<'a>> {
    parse_boxes(data).into_iter().find(|entry| &entry.kind == kind)
}

fn u32_at(data: &[u8], offset: usize) -> u32 {
    u32::from_be_bytes(data[offset..offset + 4].try_into().unwrap())
}

fn traks(bytes: &[u8]) -> Vec<BoxRef<'_>> {
    let moov = find_box(bytes, b"moov").expect("moov");
    parse_boxes(moov.payload).into_iter().filter(|entry| &entry.kind == b"trak").collect()
}

fn handler_of(trak: &BoxRef<'_>) -> [u8; 4] {
    let mdia = find_box(trak.payload, b"mdia").expect("mdia");
    let hdlr = find_box(mdia.payload, b"hdlr").expect("hdlr");
    hdlr.payload[8..12].try_into().unwrap()
}

fn stbl_of<'a>(trak: &'a BoxRef<'a>) -> BoxRef<'a> {
    let mdia = find_box(trak.payload, b"mdia").expect("mdia");
    let minf = find_box(mdia.payload, b"minf").expect("minf");
    find_box(minf.payload, b"stbl").expect("stbl")
}

fn sample_entry<'a>(stbl: &'a BoxRef<'a>) -> BoxRef<'a> {
    let stsd = find_box(stbl.payload, b"stsd").expect("stsd");
    parse_boxes(&stsd.payload[8..]).into_iter().next().expect("sample entry")
}

fn mdhd_of<'a>(trak: &'a BoxRef<'a>) -> BoxRef<'a> {
    let mdia = find_box(trak.payload, b"mdia").expect("mdia");
    find_box(mdia.payload, b"mdhd").expect("mdhd")
}

fn stco(stbl: &BoxRef<'_>) -> Vec<u32> {
    let box_ = find_box(stbl.payload, b"stco").expect("stco");
    (0..u32_at(box_.payload, 4) as usize).map(|index| u32_at(box_.payload, 8 + index * 4)).collect()
}

fn stsz(stbl: &BoxRef<'_>) -> Vec<u32> {
    let box_ = find_box(stbl.payload, b"stsz").expect("stsz");
    (0..u32_at(box_.payload, 8) as usize).map(|index| u32_at(box_.payload, 12 + index * 4)).collect()
}

fn stts_sum(stbl: &BoxRef<'_>) -> u32 {
    let box_ = find_box(stbl.payload, b"stts").expect("stts");
    let count = u32_at(box_.payload, 4) as usize;
    let mut sum = 0u32;
    for index in 0..count {
        let repeats = u32_at(box_.payload, 8 + index * 8);
        let delta = u32_at(box_.payload, 12 + index * 8);
        sum = sum.saturating_add(repeats.saturating_mul(delta));
    }
    sum
}

fn stss(stbl: &BoxRef<'_>) -> Vec<u32> {
    let box_ = find_box(stbl.payload, b"stss").expect("stss");
    (0..u32_at(box_.payload, 4) as usize).map(|index| u32_at(box_.payload, 8 + index * 4)).collect()
}

fn split_annexb(data: &[u8]) -> Vec<Vec<u8>> {
    let mut nals = Vec::new();
    let mut starts = Vec::new();
    let mut index = 0usize;
    while index + 3 <= data.len() {
        if data[index] == 0 && data[index + 1] == 0 && data[index + 2] == 1 {
            starts.push((index, index + 3));
            index += 3;
            continue;
        }
        if index + 4 <= data.len()
            && data[index] == 0
            && data[index + 1] == 0
            && data[index + 2] == 0
            && data[index + 3] == 1
        {
            starts.push((index, index + 4));
            index += 4;
            continue;
        }
        index += 1;
    }
    for (position, (_, body)) in starts.iter().enumerate() {
        let end = starts.get(position + 1).map_or(data.len(), |(next, _)| *next);
        let mut nal = &data[*body..end];
        while let Some((&0, rest)) = nal.split_last() {
            nal = rest;
        }
        if !nal.is_empty() {
            nals.push(nal.to_vec());
        }
    }
    nals
}

fn annexb(nal: &[u8]) -> Vec<u8> {
    let mut out = vec![0, 0, 0, 1];
    out.extend_from_slice(nal);
    out
}

fn synthetic_video_au() -> Vec<u8> {
    let mut out = annexb(SPS);
    out.extend_from_slice(&annexb(PPS));
    out.extend_from_slice(&annexb(&[0x65, 0x88, 0x84]));
    out
}

fn write_video_only(aus: &[Vec<u8>], fps: u32, width: u32, height: u32, path: &Path) -> Vec<u8> {
    let mut writer = PlayableContainerWriter::new(ContainerConfig {
        width,
        height,
        fps,
        video_codec: VideoCodec::H264,
        audio: None,
        max_size_bytes: 50 * 1024 * 1024,
    });
    for (index, au) in aus.iter().enumerate() {
        let keyframe = split_annexb(au).iter().any(|nal| nal[0] & 0x1f == 5);
        let pts_us = index as u64 * 1_000_000 / u64::from(fps.max(1));
        writer.write_video_sample(au, pts_us, keyframe).expect("write video sample");
    }
    let bytes = writer.finalize().expect("finalize video container");
    std::fs::write(path, &bytes).expect("write mp4");
    bytes
}

#[test]
fn ffprobe_h264_x264_acceptance() {
    let (Some(ffmpeg), Some(ffprobe)) = (ffmpeg_bin(), ffprobe_bin()) else {
        eprintln!("skipping: ffmpeg/ffprobe not available");
        return;
    };
    let raw_path = temp_path("x264.h264");
    let mp4_path = temp_path("x264.mp4");

    let _ = run_ffmpeg(
        &ffmpeg,
        &[
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=640x360:rate=30",
            "-frames:v",
            "60",
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-tune",
            "zerolatency",
            "-profile:v",
            "baseline",
            "-pix_fmt",
            "yuv420p",
            "-x264-params",
            "repeat-headers=1:aud=1:scenecut=0",
            "-f",
            "h264",
            "-y",
            raw_path.to_str().expect("utf8 path"),
        ],
    );
    let raw = std::fs::read(&raw_path).expect("read x264 Annex-B");
    assert!(!raw.is_empty());

    // AUD-деlimитеры x264 дают границы access units.
    let mut access_units: Vec<Vec<u8>> = Vec::new();
    let mut current: Vec<u8> = Vec::new();
    for nal in split_annexb(&raw) {
        if nal[0] & 0x1f == 9 && !current.is_empty() {
            access_units.push(std::mem::take(&mut current));
        }
        current.extend_from_slice(&annexb(&nal));
    }
    if !current.is_empty() {
        access_units.push(current);
    }
    assert_eq!(access_units.len(), 60, "expected 60 x264 access units");
    assert!(split_annexb(&access_units[0]).iter().any(|nal| nal[0] & 0x1f == 7), "first AU has SPS");
    assert!(split_annexb(&access_units[0]).iter().any(|nal| nal[0] & 0x1f == 8), "first AU has PPS");

    let bytes = write_video_only(&access_units, 30, 640, 360, &mp4_path);

    let fields = probe(
        &ffprobe,
        &mp4_path,
        "v:0",
        "stream=codec_name,codec_tag_string,is_avc,nal_length_size,time_base,avg_frame_rate,nb_frames,nb_read_frames,profile,width,height,duration",
    );
    eprintln!(
        "ffprobe h264: codec={:?} tag={:?} is_avc={:?} nal_length_size={:?} time_base={:?} profile={:?} frames={:?} duration={:?}",
        fields.get("codec_name"),
        fields.get("codec_tag_string"),
        fields.get("is_avc"),
        fields.get("nal_length_size"),
        fields.get("time_base"),
        fields.get("profile"),
        fields.get("nb_read_frames"),
        fields.get("duration")
    );
    assert_eq!(fields.get("codec_name").map(String::as_str), Some("h264"));
    assert_eq!(fields.get("codec_tag_string").map(String::as_str), Some("avc1"));
    assert_eq!(fields.get("is_avc").map(String::as_str), Some("true"), "Annex-B in mdat would fail this");
    assert_eq!(fields.get("nal_length_size").map(String::as_str), Some("4"));
    assert_eq!(fields.get("time_base").map(String::as_str), Some("1/90000"));
    assert_eq!(fields.get("width").map(String::as_str), Some("640"));
    assert_eq!(fields.get("height").map(String::as_str), Some("360"));
    assert_eq!(fields.get("nb_read_frames").map(String::as_str), Some("60"));
    assert!(
        fields.get("profile").is_some_and(|profile| profile.contains("Baseline")),
        "unexpected profile: {:?}",
        fields.get("profile")
    );
    let duration: f64 = fields.get("duration").and_then(|value| value.parse().ok()).expect("duration");
    assert!((duration - 2.0).abs() < 0.05, "duration {duration} != 2.0");
    if let Some(rate) = fields.get("avg_frame_rate") {
        let (num, den) = rate.split_once('/').expect("avg_frame_rate a/b");
        let ratio = num.parse::<f64>().unwrap() / den.parse::<f64>().unwrap();
        assert!((ratio - 30.0).abs() < 0.01, "avg_frame_rate {rate}");
    }

    // avcC: child of avc1, lengthSizeMinusOne = 3, SPS/PPS из потока.
    let video = traks(&bytes).into_iter().find(|trak| &handler_of(trak) == b"vide").expect("video trak");
    let table = stbl_of(&video);
    let entry = sample_entry(&table);
    assert_eq!(&entry.kind, b"avc1");
    let avcc = find_box(&entry.payload[78..], b"avcC").expect("avcC child of avc1");
    assert_eq!(avcc.payload[0], 1);
    assert_eq!(avcc.payload[4] & 0x03, 3, "nal_length_size 4");
    let first_nals = split_annexb(&access_units[0]);
    let stream_sps = first_nals.iter().find(|nal| nal[0] & 0x1f == 7).expect("stream SPS");
    let stream_pps = first_nals.iter().find(|nal| nal[0] & 0x1f == 8).expect("stream PPS");
    let sps_len = u16::from_be_bytes([avcc.payload[6], avcc.payload[7]]) as usize;
    assert_eq!(&avcc.payload[8..8 + sps_len], stream_sps.as_slice(), "avcC carries stream SPS without start code");
    let pps_base = 8 + sps_len;
    let pps_len = u16::from_be_bytes([avcc.payload[pps_base + 1], avcc.payload[pps_base + 2]]) as usize;
    assert_eq!(&avcc.payload[pps_base + 3..pps_base + 3 + pps_len], stream_pps.as_slice());

    // stco/stsz: каждый offset указывает на 4-byte length + sample, суммы сходятся.
    let offsets = stco(&table);
    let sizes = stsz(&table);
    let mdat = find_box(&bytes, b"mdat").expect("mdat");
    assert_eq!(offsets.len(), sizes.len());
    assert_eq!(offsets.len(), 60);
    assert_eq!(offsets[0] as usize, mdat.payload_start);
    let mut cursor = mdat.payload_start;
    for (index, size) in sizes.iter().enumerate() {
        assert_eq!(offsets[index] as usize, cursor, "stco chunk {index}");
        let end = cursor + *size as usize;
        let mut nalu = cursor;
        while nalu < end {
            let length = u32_at(&bytes, nalu) as usize;
            assert!(length > 0 && nalu + 4 + length <= end, "sample {index}: NALU length {length} overruns");
            assert_ne!(&bytes[nalu..nalu + 4], &[0, 0, 0, 1], "no Annex-B start code inside a sample");
            nalu += 4 + length;
        }
        assert_eq!(nalu, end, "sample {index}: length-prefixed NALUs fill the sample");
        cursor = end;
    }
    assert_eq!(cursor, mdat.payload_start + mdat.payload.len(), "stsz sums to mdat payload");
    let sum: u64 = sizes.iter().map(|size| u64::from(*size)).sum();
    assert_eq!(sum as usize, mdat.payload.len());

    // stts sum == mdhd duration (video timescale 90000).
    let mdhd = mdhd_of(&video);
    assert_eq!(u32_at(mdhd.payload, 12), 90_000);
    assert_eq!(stts_sum(&table), u32_at(mdhd.payload, 16), "mdhd duration == sum(stts)");
    assert_eq!(stss(&table).first().copied(), Some(1), "first sample is sync; no fake entry");

    assert!(decode_ok(&ffmpeg, &mp4_path, "0:v:0"), "full H.264 decode must succeed");
    let _ = std::fs::remove_file(raw_path);
    let _ = std::fs::remove_file(mp4_path);
}

fn parse_adts(data: &[u8]) -> Vec<Vec<u8>> {
    let mut frames = Vec::new();
    let mut cursor = 0usize;
    while cursor + 7 <= data.len() {
        assert_eq!(data[cursor], 0xFF, "ADTS sync");
        assert_eq!(data[cursor + 1] & 0xF0, 0xF0, "ADTS sync");
        let protection_absent = data[cursor + 1] & 0x01 == 1;
        let frame_len = ((usize::from(data[cursor + 3]) & 0x03) << 11)
            | (usize::from(data[cursor + 4]) << 3)
            | (usize::from(data[cursor + 5]) >> 5);
        let header_len = if protection_absent { 7 } else { 9 };
        assert!(frame_len >= header_len && cursor + frame_len <= data.len(), "ADTS frame length");
        frames.push(data[cursor + header_len..cursor + frame_len].to_vec());
        cursor += frame_len;
    }
    frames
}

#[test]
fn ffprobe_aac_esds_acceptance() {
    let (Some(ffmpeg), Some(ffprobe)) = (ffmpeg_bin(), ffprobe_bin()) else {
        eprintln!("skipping: ffmpeg/ffprobe not available");
        return;
    };
    let mp4_path = temp_path("aac.mp4");
    let raw = run_ffmpeg(
        &ffmpeg,
        &[
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000",
            "-t",
            "1.024",
            "-ac",
            "2",
            "-c:a",
            "aac",
            "-b:a",
            "96k",
            "-f",
            "adts",
            "pipe:1",
        ],
    );
    let frames = parse_adts(&raw);
    assert!(frames.len() >= 48, "expected >= 48 AAC frames, got {}", frames.len());

    let mut writer = PlayableContainerWriter::new(ContainerConfig {
        width: 640,
        height: 360,
        fps: 30,
        video_codec: VideoCodec::H264,
        // AAC-LC 48 kHz stereo: AudioSpecificConfig 0x11 0x90.
        audio: Some(AudioConfig::aac_lc(48_000, 2, [0x11, 0x90])),
        max_size_bytes: 50 * 1024 * 1024,
    });
    writer.write_video_sample(&synthetic_video_au(), 0, true).expect("synthetic video sample");
    for (index, frame) in frames.iter().enumerate() {
        let pts_us = index as u64 * 1_024 * 1_000_000 / 48_000;
        writer.write_audio_sample(frame, pts_us).expect("write AAC frame");
    }
    let bytes = writer.finalize().expect("finalize AAC container");
    std::fs::write(&mp4_path, &bytes).expect("write mp4");

    let fields = probe(
        &ffprobe,
        &mp4_path,
        "a:0",
        "stream=codec_name,profile,time_base,nb_read_frames,duration,channels,sample_rate",
    );
    eprintln!(
        "ffprobe aac: codec={:?} profile={:?} time_base={:?} frames={:?} duration={:?}",
        fields.get("codec_name"),
        fields.get("profile"),
        fields.get("time_base"),
        fields.get("nb_read_frames"),
        fields.get("duration")
    );
    assert_eq!(fields.get("codec_name").map(String::as_str), Some("aac"));
    assert_eq!(fields.get("profile").map(String::as_str), Some("LC"));
    assert_eq!(fields.get("time_base").map(String::as_str), Some("1/48000"));
    assert_eq!(fields.get("channels").map(String::as_str), Some("2"));
    assert_eq!(fields.get("sample_rate").map(String::as_str), Some("48000"));
    assert_eq!(
        fields.get("nb_read_frames").and_then(|value| value.parse::<usize>().ok()),
        Some(frames.len()),
        "all AAC frames must be present"
    );
    let expected_duration = frames.len() as f64 * 1_024.0 / 48_000.0;
    let duration: f64 = fields.get("duration").and_then(|value| value.parse().ok()).expect("aac duration");
    assert!((duration - expected_duration).abs() < 0.05, "duration {duration} != {expected_duration}");

    // esds: mp4a child, tags 0x03/0x04/0x05/0x06, ASC 0x11 0x90 preserved.
    let audio = traks(&bytes).into_iter().find(|trak| &handler_of(trak) == b"soun").expect("audio trak");
    let table = stbl_of(&audio);
    let entry = sample_entry(&table);
    assert_eq!(&entry.kind, b"mp4a");
    let esds = find_box(&entry.payload[28..], b"esds").expect("esds child of mp4a");
    assert_eq!(esds.payload[4], 0x03, "ES_Descriptor");
    assert!(esds.payload.windows(4).any(|window| window == [0x05, 0x02, 0x11, 0x90]), "ASC in esds");
    assert!(esds.payload.windows(3).any(|window| window == [0x04, 0x11, 0x40]), "DecoderConfig");
    assert!(esds.payload.windows(3).any(|window| window == [0x06, 0x01, 0x02]), "SLConfig");
    let mdhd = mdhd_of(&audio);
    assert_eq!(u32_at(mdhd.payload, 12), 48_000);
    assert_eq!(u32_at(mdhd.payload, 16), frames.len() as u32 * 1_024);
    assert_eq!(stts_sum(&table), frames.len() as u32 * 1_024);

    assert!(decode_ok(&ffmpeg, &mp4_path, "0:a:0"), "full AAC decode must succeed");
    let _ = std::fs::remove_file(mp4_path);
}
