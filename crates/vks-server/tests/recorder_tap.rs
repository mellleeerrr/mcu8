//! T09b: запись через tap **существующей** группы фермы.
//!
//! Проверяет, что:
//! - без живой H.264-группы `start` возвращает понятную ошибку и не спавнит
//!   скрытый FFmpeg/группу/recorder-участника;
//! - с группой запись подписывается на неё, не увеличивая `refcount`/`viewers`;
//! - реальные кадры доходят до MP4 (avcC/length-prefix, ffprobe);
//! - ошибка финализации не теряет RoomHub-участника и активную строку записи.

use std::sync::Arc;
use std::time::Duration;

use vks_admin::state::{AppState, RecordingControl as _};
use vks_core::track::{Track, TrackKind};
use vks_core::{ParticipantId, RoomId};
use vks_media_mcu::{EncoderSpec, FarmCodec, Frame, McuEngine};
use vks_server::{RecorderControl, RecorderManager};

fn ffmpeg_available() -> bool {
    let binary = std::env::var("VKS_FFMPEG_BIN").unwrap_or_else(|_| "ffmpeg".to_string());
    std::process::Command::new(binary)
        .arg("-version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

fn ffprobe_available() -> bool {
    std::process::Command::new("ffprobe")
        .arg("-version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

fn encoder_spec() -> EncoderSpec {
    EncoderSpec {
        codec: FarmCodec::H264,
        width: 640,
        height: 360,
        fps: 30,
        bitrate_kbps: 1024,
        maxrate_kbps: 1024,
        bufsize_kbps: 2048,
        profile: "baseline".into(),
        level: "3.1".into(),
        keyint: 30,
        encoder_params: "repeat-headers=1:aud=1:scenecut=0".into(),
    }
}

fn gray_frame() -> Arc<Frame> {
    let mut data = vec![128u8; 640 * 360 * 3 / 2];
    data[..640 * 360].fill(16);
    Arc::new(Frame::new(640, 360, data))
}

#[tokio::test]
async fn recording_start_without_group_is_rejected_and_spawns_no_hidden_group() {
    let state = Arc::new(AppState::new());
    let room = RoomId::new();
    let engine = Arc::new(McuEngine::new());
    let manager = RecorderManager::new();
    let control = RecorderControl::new(manager.clone(), Arc::downgrade(&state), engine.clone());

    let error = control.start(room).await.expect_err("no live encoder group -> no recording");
    assert!(error.contains("no_active_h264_encoder_group"), "clear 409-style error, got: {error}");
    assert!(!manager.is_recording(&room), "no session may be created");
    assert_eq!(engine.video_farm().group_count(), 0, "start must not spawn a hidden group");
    assert!(engine.video_farm_stats().is_empty(), "no hidden farm series");
    assert_eq!(control.active_h264_groups(room), 0);
    assert!(state.room_hub.list_participants(&room).is_empty(), "no recorder participant");
    assert!(state.records.lock().unwrap().is_empty(), "no bogus record row");
}

#[tokio::test]
async fn recording_tap_reuses_existing_group_without_becoming_a_viewer() {
    if !ffmpeg_available() {
        eprintln!("skipping: ffmpeg not available");
        return;
    }
    let state = Arc::new(AppState::new());
    let room = RoomId::new();
    let engine = Arc::new(McuEngine::new());
    let participant = ParticipantId::new();
    engine.bridge_participant(room, participant, vec![Track::new(participant, TrackKind::Video)]);
    assert!(engine.ensure_composite_size(room, 640, 360));
    let viewer = engine.acquire_video_viewer(room, &encoder_spec()).await.expect("existing encoder group");
    let before = engine.video_farm_stats();
    assert_eq!(before.len(), 1);
    assert_eq!(before[0].viewers, 1);

    let manager = RecorderManager::new();
    let control = RecorderControl::new(manager.clone(), Arc::downgrade(&state), engine.clone());
    let record_id = control.start(room).await.expect("start taps the existing group");
    assert!(!record_id.is_empty());
    assert!(manager.is_recording(&room));

    let after = engine.video_farm_stats();
    assert_eq!(after.len(), 1, "tap must not create a second group");
    assert_eq!(after[0].refcount, before[0].refcount, "tap is non-owning: refcount unchanged");
    assert_eq!(after[0].viewers, 1, "tap is not a viewer");
    assert_eq!(engine.video_farm().group_count(), 1);
    let participants = state.room_hub.list_participants(&room);
    assert_eq!(participants.len(), 1, "only the recorder participant is registered");
    assert!(participants[0].is_recorder);

    for _ in 0..45 {
        let _ = engine.publish_video_frame(room, &gray_frame());
        tokio::time::sleep(Duration::from_millis(34)).await;
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    control.stop(room).await.expect("stop finalizes a non-empty container");
    assert!(!manager.is_recording(&room));
    assert!(state.room_hub.list_participants(&room).is_empty());

    let bytes = manager.get_container(&record_id).expect("container saved by the tap");
    assert!(bytes.len() > 64);
    assert_eq!(&bytes[4..8], b"ftyp");
    let saved = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("t09b_tap_{record_id}.mp4"));
    std::fs::write(&saved, &bytes).expect("write tap mp4");

    let row = state.records.lock().unwrap().get(&record_id).cloned().expect("record row");
    assert!(row.duration_sec >= 1, "finished row must not look active: {row:?}");
    assert_eq!(row.size_bytes, bytes.len() as u64, "row size is the real MP4 size");
    assert_eq!(
        (row.width, row.height, row.bitrate_kbps),
        (640, 360, 1024),
        "actual group spec, not the default profile"
    );

    // End-to-end: the tapped file is a real avc1 MP4 and fully decodes.
    if ffprobe_available() {
        let output = std::process::Command::new("ffprobe")
            .args([
                "-v",
                "error",
                "-count_frames",
                "-select_streams",
                "v:0",
                "-show_entries",
                "stream=codec_name,codec_tag_string,is_avc,nal_length_size,time_base,nb_read_frames",
                "-of",
                "default=noprint_wrappers=1",
            ])
            .arg(&saved)
            .output()
            .expect("ffprobe");
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        let text = String::from_utf8_lossy(&output.stdout).to_string();
        eprintln!("tap ffprobe:\n{text}");
        for expected in
            ["codec_name=h264", "codec_tag_string=avc1", "is_avc=true", "nal_length_size=4", "time_base=1/90000"]
        {
            assert!(text.lines().any(|line| line == expected), "missing {expected} in {text}");
        }
        let frames = text
            .lines()
            .find_map(|line| line.strip_prefix("nb_read_frames="))
            .and_then(|value| value.parse::<usize>().ok())
            .expect("nb_read_frames");
        assert!(frames > 10, "expected real frames from the group, got {frames}");
        let decode = std::process::Command::new(std::env::var("VKS_FFMPEG_BIN").unwrap_or_else(|_| "ffmpeg".into()))
            .args(["-v", "error", "-i"])
            .arg(&saved)
            .args(["-map", "0:v:0", "-f", "null", "-"])
            .output()
            .expect("ffmpeg decode");
        assert!(decode.status.success(), "tap file must decode: {}", String::from_utf8_lossy(&decode.stderr));
    }
    let _ = std::fs::remove_file(saved);
    drop(viewer);
}

#[tokio::test]
async fn recording_stop_surfaces_container_error_but_still_detaches_room_hub() {
    if !ffmpeg_available() {
        eprintln!("skipping: ffmpeg not available");
        return;
    }
    let state = Arc::new(AppState::new());
    let room = RoomId::new();
    let engine = Arc::new(McuEngine::new());
    let participant = ParticipantId::new();
    engine.bridge_participant(room, participant, vec![Track::new(participant, TrackKind::Video)]);
    assert!(engine.ensure_composite_size(room, 640, 360));
    let viewer = engine.acquire_video_viewer(room, &encoder_spec()).await.expect("existing encoder group");

    let manager = RecorderManager::new();
    let control = RecorderControl::new(manager.clone(), Arc::downgrade(&state), engine.clone());
    let record_id = control.start(room).await.expect("start");
    // No frames published: the tap never writes a sample, finalize must fail
    // with `Empty` instead of returning a 968-byte "valid" MP4.
    let error = control.stop(room).await.expect_err("empty tap must surface a finalize error");
    assert!(error.contains("контейнер пуст") || error.contains("Empty"), "unexpected error: {error}");
    assert!(!manager.is_recording(&room));
    assert!(manager.get_container(&record_id).is_none(), "no bogus empty artifact");
    assert!(state.room_hub.list_participants(&room).is_empty(), "RoomHub participant removed despite the error");
    let row = state.records.lock().unwrap().get(&record_id).cloned().expect("row");
    assert!(row.duration_sec >= 1, "row must be finalized (not active) despite the error: {row:?}");
    assert_eq!(row.size_bytes, 0);
    drop(viewer);
}
