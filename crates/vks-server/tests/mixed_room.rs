//! Real-media end-to-end test for the T12 hybrid browser ↔ SIP/H.323 audio bridge.

use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::net::UdpSocket;
use tokio::sync::mpsc;

use vks_admin::rooms::Rooms;
use vks_admin::state::AppState;
use vks_core::ports::media_engine::{MediaEngine, RoomCaps, RoomMediaCfg};
use vks_core::{ParticipantId, RoomMode};
use vks_gateway_sip::opus::{OpusCodec, FRAME_SAMPLES};
use vks_gateway_sip::rtp_bridge::write_rtp_header;
use vks_gateway_sip::RtpPortPool;
use vks_media_mcu::McuEngine;
use vks_media_sfu::str0m::change::{SdpAnswer, SdpPendingOffer};
use vks_media_sfu::str0m::media::{Direction, MediaKind as Str0mMediaKind, MediaTime};
use vks_media_sfu::str0m::net::{Protocol, Receive};
use vks_media_sfu::str0m::{Candidate, Input, Output, RtcConfig};
use vks_media_sfu::MediaRegistry;
use vks_server::bridge;
use vks_server::gateways::{GatewayMediaControls, GatewayMediaStats, RtpBridgeSession};
use vks_server::webrtc_runtime;
use vks_server::{gateways, parse_participant_id};
use vks_signaling::handler::HandlerConfig;

fn tone(samples: usize, hz: f32, start: usize) -> Vec<f32> {
    (0..samples).map(|i| (2.0 * std::f32::consts::PI * hz * (start + i) as f32 / 48_000.0).sin() * 0.3).collect()
}

fn goertzel_energy(samples: &[f32], freq: f32) -> f32 {
    let omega = 2.0 * std::f32::consts::PI * freq / 48_000.0;
    let coeff = 2.0 * omega.cos();
    let (mut s1, mut s2) = (0.0f32, 0.0f32);
    for &x in samples {
        let s0 = x + coeff * s1 - s2;
        s2 = s1;
        s1 = s0;
    }
    let power = s1 * s1 + s2 * s2 - coeff * s1 * s2;
    power / samples.len().max(1) as f32
}

fn parse_candidate_addr(sdp: &str) -> Option<SocketAddr> {
    for line in sdp.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix("a=candidate:") else {
            continue;
        };
        let parts: Vec<&str> = rest.split_whitespace().collect();
        for i in 0..parts.len().saturating_sub(1) {
            if let Ok(ip) = parts[i].parse::<IpAddr>() {
                if let Ok(port) = parts[i + 1].parse::<u16>() {
                    return Some(SocketAddr::new(ip, port));
                }
            }
        }
    }
    None
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mixed_room_audio_bridge_end_to_end() {
    let state = Arc::new(AppState::new());
    let mcu = Arc::new(McuEngine::new());
    let sfu_engine = Arc::new(MediaRegistry::new());

    let room_record = Rooms::create(&state, "mixed");
    let room = room_record.id;
    let cfg = RoomMediaCfg { room_id: room, mode: RoomMode::Mcu, caps: RoomCaps::default() };
    sfu_engine.create_room(cfg.clone()).await.expect("sfu create room");
    mcu.create_room(cfg).await.expect("mcu create room");

    let client_socket = Arc::new(UdpSocket::bind("127.0.0.1:0").await.expect("client socket"));
    let client_local = client_socket.local_addr().expect("client local");
    let mut client = RtcConfig::new().set_ice_lite(false).build(Instant::now());
    client.add_local_candidate(Candidate::host(client_local, "udp").expect("host candidate"));

    let mut change = client.sdp_api();
    let audio_mid = change.add_media(Str0mMediaKind::Audio, Direction::SendRecv, None, None, None);
    let (offer, pending): (_, SdpPendingOffer) = change.apply().expect("offer");

    let handler_config = HandlerConfig::default_test();
    let pid = ParticipantId::new();
    let accepted =
        webrtc_runtime::accept_offer(&handler_config, room, pid, &offer.to_sdp_string()).await.expect("accept offer");

    let tracks = vec![vks_core::track::Track::new(pid, vks_core::track::TrackKind::Audio)];
    let _ = sfu_engine.add_publisher(room, pid, tracks.clone()).await;
    let _ = mcu.add_publisher(room, pid, tracks.clone()).await;
    mcu.seat_publisher(room, pid, tracks.clone());

    let server_candidate = parse_candidate_addr(&accepted.answer_sdp).expect("server candidate");
    let answer = SdpAnswer::from_sdp_string(&accepted.answer_sdp).expect("answer parses");
    client.sdp_api().accept_answer(pending, answer).expect("accept answer");

    let pt = client.writer(audio_mid).expect("audio writer").payload_params().next().expect("payload params").pt();

    let gw_uuid = uuid::Uuid::new_v4();
    let gw_id_str = format!("sip-{}@sip-gateway", gw_uuid);
    let gw_pid = parse_participant_id(&gw_id_str);
    assert!(gateways::land_gateway_participant_in_room(&state, Some(&mcu), &room, &gw_id_str, "SIP", false));
    let terminal_socket = Arc::new(UdpSocket::bind("127.0.0.1:0").await.expect("terminal socket"));
    let term_port = terminal_socket.local_addr().expect("term").port();
    let gateway_socket = UdpSocket::bind("127.0.0.1:0").await.expect("gateway socket");
    let gateway_port = gateway_socket.local_addr().expect("gw").port();
    let cancel = Arc::new(AtomicBool::new(false));
    let (_tx, dtmf_rx) = mpsc::unbounded_channel();
    let task = gateways::spawn_rtp_media_bridge(RtpBridgeSession {
        room_id: room,
        participant_id_str: gw_id_str.clone(),
        participant_id_val: gw_pid,
        remote_media_ip: "127.0.0.1".into(),
        remote_audio_port: term_port,
        audio_payload_type: 111,
        audio_codec: Some("OPUS"),
        source_latch: false,
        ip_latch: false,
        dtmf_payload_type: None,
        dtmf_events: None,
        media_stats: Arc::new(GatewayMediaStats::default()),
        rtp_socket: gateway_socket,
        rtcp_socket: None,
        rtp_port: gateway_port,
        port_pool: Arc::new(RtpPortPool::new(49000, 49002)),
        mcu_engine: mcu.clone(),
        controls: GatewayMediaControls::default(),
        cancel_flag: cancel.clone(),
        dtmf_rx,
    });

    tokio::time::sleep(Duration::from_millis(150)).await;
    assert!(bridge::bridge_active(room), "bridge must be active");
    assert!(bridge::bridge_owns_tick(room), "bridge must own the tick");
    assert!(mcu.bridge_owns_tick(room));

    let browser_payloads: Arc<Mutex<Vec<Vec<u8>>>> = Arc::new(Mutex::new(Vec::new()));
    let terminal_payloads: Arc<Mutex<Vec<Vec<u8>>>> = Arc::new(Mutex::new(Vec::new()));

    let ticks_start = mcu.audio_ticks_total();

    let exchange_start = Instant::now();
    let mut now;
    let mut connected = false;
    let mut seq_gw: u16 = 1;
    let mut ts_gw: u32 = 0;
    let mut frame_idx: u64 = 0;
    let mut buf = vec![0u8; 65535];

    while exchange_start.elapsed() < Duration::from_secs(2) {
        now = Instant::now();

        {
            let pcm = tone(FRAME_SAMPLES, 1000.0, frame_idx as usize * FRAME_SAMPLES);
            let mut codec = OpusCodec::new().expect("opus");
            let packet = codec.encode(&pcm).expect("encode");
            let media_time = MediaTime::from_millis(frame_idx * 20);
            let _ = client.writer(audio_mid).expect("writer").write(pt, now, media_time, packet);
        }

        client.handle_input(Input::Timeout(now)).ok();

        loop {
            match client.poll_output() {
                Ok(Output::Timeout(_)) => break,
                Ok(Output::Transmit(t)) => {
                    let _ = client_socket.send_to(&t.contents, server_candidate).await;
                }
                Ok(Output::Event(vks_media_sfu::str0m::Event::Connected)) => {
                    connected = true;
                }
                Ok(Output::Event(vks_media_sfu::str0m::Event::MediaData(m))) => {
                    browser_payloads.lock().unwrap().push(m.data.to_vec());
                }
                Ok(Output::Event(_)) => {}
                Err(_) => break,
            }
        }

        while let Ok((n, src)) = client_socket.try_recv_from(&mut buf) {
            let recv = Receive::new(Protocol::Udp, src, client_local, &buf[..n]).expect("receive");
            let _ = client.handle_input(Input::Receive(now, recv));
        }

        {
            let pcm = tone(FRAME_SAMPLES, 440.0, frame_idx as usize * FRAME_SAMPLES);
            let mut codec = OpusCodec::new().expect("opus");
            let payload = codec.encode(&pcm).expect("encode");
            let total = 12 + payload.len();
            let mut packet = vec![0u8; total];
            write_rtp_header(&mut packet[..12], 111, seq_gw, ts_gw, 1, false);
            packet[12..].copy_from_slice(&payload);
            let dest: SocketAddr = format!("127.0.0.1:{}", gateway_port).parse().unwrap();
            let _ = terminal_socket.send_to(&packet, dest).await;
            seq_gw = seq_gw.wrapping_add(1);
            ts_gw = ts_gw.wrapping_add(FRAME_SAMPLES as u32);
        }

        let mut tbuf = [0u8; 65535];
        while let Ok((n, _src)) = terminal_socket.try_recv_from(&mut tbuf) {
            if n > 12 {
                terminal_payloads.lock().unwrap().push(tbuf[12..n].to_vec());
            }
        }

        frame_idx += 1;
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    assert!(connected, "client must connect");

    let mut term_codec = OpusCodec::new().expect("opus");
    let mut term_pcm: Vec<f32> = Vec::new();
    for p in terminal_payloads.lock().unwrap().iter() {
        if let Some(decoded) = term_codec.decode(p) {
            term_pcm.extend(decoded);
        }
    }
    let mut browser_codec = OpusCodec::new().expect("opus");
    let mut browser_pcm: Vec<f32> = Vec::new();
    for p in browser_payloads.lock().unwrap().iter() {
        if let Some(decoded) = browser_codec.decode(p) {
            browser_pcm.extend(decoded);
        }
    }

    if !term_pcm.is_empty() {
        let e1k = goertzel_energy(&term_pcm, 1000.0);
        let e440 = goertzel_energy(&term_pcm, 440.0);
        assert!(
            e1k > 1e-4 && e1k > e440,
            "terminal must receive 1 kHz energy (1k={e1k}, 440={e440}, n={})",
            term_pcm.len()
        );
    } else {
        // Soft fail: at least require some frames in one direction; but test should fail.
        panic!("no terminal payloads received");
    }
    if browser_pcm.is_empty() {
        panic!("no browser (bridge egress) payloads received");
    } else {
        let e440 = goertzel_energy(&browser_pcm, 440.0);
        let e1k = goertzel_energy(&browser_pcm, 1000.0);
        assert!(
            e440 > 1e-4 && e440 > e1k,
            "browser must receive 440 Hz energy, no 1 kHz echo (440={e440}, 1k={e1k}, n={})",
            browser_pcm.len()
        );
    }

    let ticks_end = mcu.audio_ticks_total();
    let growth = ticks_end.saturating_sub(ticks_start);
    assert!((40..=220).contains(&growth), "tick growth should be ~50/s not ~100/s (was {growth})");

    cancel.store(true, Ordering::Relaxed);
    let _ = tokio::time::timeout(Duration::from_millis(500), task).await;
    gateways::terminate_gateway_participant_in_room(&state, Some(&mcu), &room, &gw_id_str);
    for _ in 0..20 {
        if !bridge::bridge_active(room) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(!bridge::bridge_active(room), "bridge must stop on teardown");
    assert!(!bridge::bridge_owns_tick(room), "tick ownership must be released");
    assert!(!mcu.bridge_owns_tick(room));

    let _ = (sfu_engine, gw_pid, _tx);
}
