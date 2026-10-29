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
    encoder_spec_with_bitrate(1024)
}

/// Второй `EncoderSpec` на том же холсте — другой ключ группы (re-key).
fn encoder_spec_with_bitrate(bitrate_kbps: u32) -> EncoderSpec {
    EncoderSpec {
        codec: FarmCodec::H264,
        width: 640,
        height: 360,
        fps: 30,
        bitrate_kbps,
        maxrate_kbps: bitrate_kbps,
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
                "stream=codec_name,codec_tag_string,is_avc,nal_length_size,time_base,has_b_frames,nb_read_frames",
                "-of",
                "default=noprint_wrappers=1",
            ])
            .arg(&saved)
            .output()
            .expect("ffprobe");
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        let text = String::from_utf8_lossy(&output.stdout).to_string();
        eprintln!("tap ffprobe:\n{text}");
        for expected in [
            "codec_name=h264",
            "codec_tag_string=avc1",
            "is_avc=true",
            "nal_length_size=4",
            "time_base=1/90000",
            // T09b review E P3-7: B-кадров нет, `ctts` не пишется — MP4 не
            // должен нести B-frames (перепроверка независимым декодером).
            "has_b_frames=0",
        ] {
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

/// T09b review A P1-1: группа ретировалась (ушёл последний зритель) — tap
/// завершается сам, запись не должна оставаться ACTIVE/Running без источника,
/// а повторный start должен начать новую запись, а не «успех без кадров».
#[tokio::test]
async fn recording_finalizes_when_encoder_group_closes() {
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
    let viewer = engine.acquire_video_viewer(room, &encoder_spec()).await.expect("encoder group");

    let manager = RecorderManager::new();
    let control = RecorderControl::new(manager.clone(), Arc::downgrade(&state), engine.clone());
    let record_id = control.start(room).await.expect("start");
    for _ in 0..30 {
        let _ = engine.publish_video_frame(room, &gray_frame());
        tokio::time::sleep(Duration::from_millis(34)).await;
    }
    tokio::time::sleep(Duration::from_millis(200)).await;
    // Последний зритель уходит: broadcast закрывается, tap естественно выходит.
    drop(viewer);
    let mut finalized = false;
    for _ in 0..100 {
        if !manager.is_recording(&room) {
            finalized = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(finalized, "recording must finalize when the encoder group closes");
    assert!(state.room_hub.list_participants(&room).is_empty(), "recorder participant detached");
    let row = state.records.lock().unwrap().get(&record_id).cloned().expect("record row");
    assert!(row.duration_sec >= 1, "row must not look active after group loss: {row:?}");
    assert!(manager.get_container(&record_id).is_some(), "recorded prefix saved as MP4");

    // Новая группа и повторный start — новая запись, без «already running».
    let viewer2 = engine.acquire_video_viewer(room, &encoder_spec()).await.expect("new encoder group");
    let _ = engine.publish_video_frame(room, &gray_frame());
    let second = control.start(room).await.expect("start after group loss");
    assert_ne!(second, record_id, "a new session must be created, not a stale one");
    control.stop(room).await.ok();
    drop(viewer2);
}

/// T09b review E P2-1: в комнате остаётся вторая живая H.264-группа B, а
/// группа A, на которую подписан tap, закрывается. Немедленный `start` не
/// имеет права вернуть Ok(stale id): живость проверяется по собственному
/// ключу tap'а, а не по `active_h264_groups(room)`. После автофинализации B
/// должна остаться доступной для tap'а.
#[tokio::test]
async fn recording_immediate_restart_rejects_dead_tap_with_second_live_group() {
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

    let viewer_a = engine.acquire_video_viewer(room, &encoder_spec()).await.expect("group A");
    let manager = RecorderManager::new();
    let control = RecorderControl::new(manager.clone(), Arc::downgrade(&state), engine.clone());
    let old_id = control.start(room).await.expect("start taps group A");
    assert!(manager.is_recording(&room));

    // Вторая живая группа того же холста, но другого EncoderSpec (re-key):
    // её наличие маскировало мёртвый tap при проверке active_h264_groups.
    let viewer_b = engine.acquire_video_viewer(room, &encoder_spec_with_bitrate(512)).await.expect("group B");
    assert_eq!(engine.video_farm().active_group_count(room, FarmCodec::H264), 2);
    assert_eq!(engine.video_farm_stats().len(), 2, "two live groups in one room");

    // Последний зритель A уходит: A ретируется синхронно, B остаётся живой.
    drop(viewer_a);
    assert_eq!(engine.video_farm().active_group_count(room, FarmCodec::H264), 1, "only B remains live");

    // Окно автофинализации: сессия ещё жива, но её tap мёртв. Ложный 200 со
    // старым id запрещён, даже несмотря на живую B.
    let error = control.start(room).await.expect_err("dead tap must not return a stale id while B lives");
    assert!(error.contains("запись завершается"), "same honest error text, got: {error}");
    assert!(manager.is_recording(&room), "stale session is still in its finalize window");

    // Detached finalize старой сессии: пустой MP4 не пишется, B не трогается.
    let mut finalized = false;
    for _ in 0..100 {
        if !manager.is_recording(&room) {
            finalized = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(finalized, "dead tap must finalize the stale session");
    assert!(manager.get_container(&old_id).is_none(), "no empty MP4 for the dead tap");
    assert_eq!(engine.video_farm().active_group_count(room, FarmCodec::H264), 1, "B survived the stale finalize");
    let old_row = state.records.lock().unwrap().get(&old_id).cloned().expect("old record row");
    assert!(old_row.duration_sec >= 1, "old row must not look active: {old_row:?}");
    assert_eq!(old_row.size_bytes, 0, "dead tap wrote nothing: {old_row:?}");

    // B остаётся tap-able: новый start подписывается уже на неё.
    let new_id = control.start(room).await.expect("B is live and must be tappable");
    assert_ne!(new_id, old_id, "a new session for B, not the stale id");
    for _ in 0..45 {
        let _ = engine.publish_video_frame(room, &gray_frame());
        tokio::time::sleep(Duration::from_millis(34)).await;
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    control.stop(room).await.expect("new tap writes a non-empty MP4 from B");
    let bytes = manager.get_container(&new_id).expect("B tap saved an MP4");
    assert!(bytes.len() > 64, "B tap produced real samples: {} bytes", bytes.len());
    let row = state.records.lock().unwrap().get(&new_id).cloned().expect("new record row");
    assert_eq!((row.width, row.height, row.bitrate_kbps), (640, 360, 512), "row uses B's spec");
    drop(viewer_b);
}

/// T09b review D/E: одиночная группа закрыта — немедленный второй `start`
/// 8/8 обязан вернуть ту же ошибку, а не Ok(stale id). Committed-версия
/// внешнего probe review E (без второй группы).
#[tokio::test]
async fn recording_immediate_restart_without_group_is_rejected_eight_of_eight() {
    if !ffmpeg_available() {
        eprintln!("skipping: ffmpeg not available");
        return;
    }
    let state = Arc::new(AppState::new());
    let engine = Arc::new(McuEngine::new());
    let manager = RecorderManager::new();
    let control = RecorderControl::new(manager.clone(), Arc::downgrade(&state), engine.clone());
    for iteration in 0..8 {
        let room = RoomId::new();
        let participant = ParticipantId::new();
        engine.bridge_participant(room, participant, vec![Track::new(participant, TrackKind::Video)]);
        assert!(engine.ensure_composite_size(room, 640, 360));
        let viewer = engine.acquire_video_viewer(room, &encoder_spec()).await.expect("encoder group");
        let record_id = control.start(room).await.expect("start");
        assert!(manager.is_recording(&room));
        drop(viewer);
        assert_eq!(engine.video_farm().active_group_count(room, FarmCodec::H264), 0, "iteration {iteration}");
        let error = control.start(room).await.expect_err("immediate restart in the finalize window");
        assert!(error.contains("запись завершается"), "iteration {iteration}: {error}");
        assert!(manager.is_recording(&room), "iteration {iteration}: session still finalizing");
        let mut finalized = false;
        for _ in 0..100 {
            if !manager.is_recording(&room) {
                finalized = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(finalized, "iteration {iteration}: dead tap did not finalize");
        assert!(manager.get_container(&record_id).is_none(), "iteration {iteration}: no empty MP4");
    }
    assert_eq!(engine.video_farm().group_count(), 0, "no hidden/leaked encoder groups");
}

/// T09b review A/B P2-1: повторный stop после ошибки финализации обязан
/// повторять ту же ошибку, а не отдавать 200/Ok.
#[tokio::test]
async fn repeated_stop_replays_finalize_error() {
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
    let viewer = engine.acquire_video_viewer(room, &encoder_spec()).await.expect("encoder group");
    let manager = RecorderManager::new();
    let control = RecorderControl::new(manager.clone(), Arc::downgrade(&state), engine.clone());
    control.start(room).await.expect("start");
    let first = control.stop(room).await.expect_err("empty recording errors");
    let second = control.stop(room).await.expect_err("repeat stop must replay the error");
    assert_eq!(first, second, "same finalize error on idempotent retry");
    drop(viewer);
}
