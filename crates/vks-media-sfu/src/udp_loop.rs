//! Асинхронный медиа-луп для WebRTC UDP-трафика (MEDIA_PIPELINE §2).
//!
//! Обеспечивает реальную работу с сетевыми UDP-сокетами:
//! - Открытие `tokio::net::UdpSocket` для приёма и передачи RTP/RTCP-трафика.
//! - Неблокирующий цикл: приём UDP-датаграмм -> обработка в SFU -> отправка адресатам.
//! - Zero-copy маршрутизация медиа-буферов через `bytes::Bytes`.
//! - Graceful shutdown по каналу отмены.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use bytes::Bytes;
use tokio::net::UdpSocket;
use tokio::sync::broadcast;
use tracing::{debug, info, warn};
use vks_core::TrackId;

use crate::forwarder::RtpPacket;

/// Статистика медиа-сокета.
#[derive(Debug, Default)]
pub struct UdpMediaStats {
    pub packets_received: AtomicU64,
    pub bytes_received: AtomicU64,
    pub packets_sent: AtomicU64,
    pub bytes_sent: AtomicU64,
    pub packets_dropped: AtomicU64,
}

/// Асинхронный сетевой медиа-луп SFU.
pub struct UdpMediaLoop {
    socket: Arc<UdpSocket>,
    local_addr: SocketAddr,
    stats: Arc<UdpMediaStats>,
    shutdown_tx: broadcast::Sender<()>,
}

impl UdpMediaLoop {
    /// Привязывает сокет к указанному адресу (например, `127.0.0.1:0` для dynamic port или `0.0.0.0:50000`).
    pub async fn bind(addr: &str) -> std::io::Result<Self> {
        let socket = UdpSocket::bind(addr).await?;
        let local_addr = socket.local_addr()?;
        let (shutdown_tx, _) = broadcast::channel(1);

        info!(%local_addr, "bound UDP media socket");

        Ok(Self { socket: Arc::new(socket), local_addr, stats: Arc::new(UdpMediaStats::default()), shutdown_tx })
    }

    /// Локальный адрес сокета.
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// Статистика передачи.
    pub fn stats(&self) -> &Arc<UdpMediaStats> {
        &self.stats
    }

    /// Отправляет сырые байты по указанному адресу.
    pub async fn send_raw(&self, dest: SocketAddr, data: &[u8]) -> std::io::Result<usize> {
        let bytes = self.socket.send_to(data, dest).await?;
        self.stats.packets_sent.fetch_add(1, Ordering::Relaxed);
        self.stats.bytes_sent.fetch_add(bytes as u64, Ordering::Relaxed);
        Ok(bytes)
    }

    /// Отправляет RTP-пакет конкретному подписчику по UDP (zero-copy).
    pub async fn send_packet(&self, dest: SocketAddr, packet: &RtpPacket) -> std::io::Result<usize> {
        self.send_raw(dest, &packet.payload).await
    }

    /// Запускает фоновый цикл приёма с автоматической маршрутизацией пакетов (с поддержкой ICE STUN).
    pub fn start_routing_loop<F>(&self, mut router: F) -> tokio::task::JoinHandle<()>
    where
        F: FnMut(SocketAddr, RtpPacket) -> Vec<(SocketAddr, Bytes)> + Send + 'static,
    {
        let socket = self.socket.clone();
        let stats = self.stats.clone();
        let mut shutdown_rx = self.shutdown_tx.subscribe();

        tokio::spawn(async move {
            let mut buf = [0u8; 2048]; // Стандартный MTU < 1500

            loop {
                tokio::select! {
                    _ = shutdown_rx.recv() => {
                        debug!("UDP media receive loop received shutdown signal");
                        break;
                    }
                    res = socket.recv_from(&mut buf) => {
                        match res {
                            Ok((size, src)) => {
                                stats.packets_received.fetch_add(1, Ordering::Relaxed);
                                stats.bytes_received.fetch_add(size as u64, Ordering::Relaxed);

                                // 1. Автоматический ответ на STUN Binding Request (RFC 5389 / RFC 8445 ICE-lite)
                                if is_stun_binding_request(&buf[..size]) {
                                    if let Some(username) = parse_stun_username(&buf[..size]) {
                                        if !username.trim().is_empty() {
                                            if let Some(resp) = build_stun_binding_response(&buf[..size], src) {
                                                let _ = socket.send_to(&resp, src).await;
                                                // Оповещаем маршрутизатор о полученном STUN пакете для auto-punching подписчиков
                                                let stun_dummy = RtpPacket::new(TrackId::default(), Bytes::new());
                                                let _ = router(src, stun_dummy);
                                                continue;
                                            }
                                        }
                                    }
                                    stats.packets_dropped.fetch_add(1, Ordering::Relaxed);
                                    continue;
                                }

                                let payload = Bytes::copy_from_slice(&buf[..size]);
                                let packet = RtpPacket::new(TrackId::default(), payload);
                                let targets = router(src, packet);
                                if targets.is_empty() {
                                    stats.packets_dropped.fetch_add(1, Ordering::Relaxed);
                                } else {
                                    for (dest, out_bytes) in targets {
                                        if let Ok(sent) = socket.send_to(&out_bytes, dest).await {
                                            stats.packets_sent.fetch_add(1, Ordering::Relaxed);
                                            stats.bytes_sent.fetch_add(sent as u64, Ordering::Relaxed);
                                        }
                                    }
                                }
                            }
                            Err(e) => {
                                warn!(error = %e, "error reading UDP media packet");
                            }
                        }
                    }
                }
            }
        })
    }

    /// Запускает фоновый цикл приёма и обработки датаграмм.
    pub fn start_receive_loop<F>(&self, mut packet_handler: F) -> tokio::task::JoinHandle<()>
    where
        F: FnMut(SocketAddr, RtpPacket) + Send + 'static,
    {
        self.start_routing_loop(move |src, packet| {
            packet_handler(src, packet);
            Vec::new()
        })
    }

    /// Инициирует остановку медиа-лупа.
    pub fn shutdown(&self) {
        let _ = self.shutdown_tx.send(());
    }
}

/// Проверяет, является ли датаграмма STUN Binding Request (RFC 5389 §6).
pub fn is_stun_binding_request(data: &[u8]) -> bool {
    data.len() >= 20 && data[0] == 0x00 && data[1] == 0x01 && data[4..8] == [0x21, 0x12, 0xa4, 0x42]
}

/// Извлекает USERNAME из STUN-сообщения без аллокаций (RFC 5389 §15.3, Type = 0x0006).
pub fn parse_stun_username(data: &[u8]) -> Option<&str> {
    if data.len() < 20 {
        return None;
    }
    let msg_len = u16::from_be_bytes([data[2], data[3]]) as usize;
    let total_len = 20 + msg_len;
    if data.len() < total_len {
        return None;
    }
    let mut offset = 20;
    while offset + 4 <= total_len {
        let attr_type = u16::from_be_bytes([data[offset], data[offset + 1]]);
        let attr_len = u16::from_be_bytes([data[offset + 2], data[offset + 3]]) as usize;
        offset += 4;
        if offset + attr_len > total_len {
            return None;
        }
        if attr_type == 0x0006 {
            let val_bytes = &data[offset..offset + attr_len];
            return std::str::from_utf8(val_bytes).ok();
        }
        // STUN attributes are aligned to 4-byte boundaries
        let padded_len = (attr_len + 3) & !3;
        offset += padded_len;
    }
    None
}

/// Проверяет, аутентифицирован ли STUN-запрос по ожидаемому ICE ufrag (RFC 8445 §7.2.1).
pub fn is_stun_request_authenticated(data: &[u8], expected_ufrag: &str) -> bool {
    if let Some(username) = parse_stun_username(data) {
        if let Some((server_ufrag, _client_ufrag)) = username.split_once(':') {
            server_ufrag == expected_ufrag
        } else {
            username == expected_ufrag
        }
    } else {
        false
    }
}

/// Формирует STUN Binding Success Response с атрибутом XOR-MAPPED-ADDRESS (RFC 5389 §15.2).
pub fn build_stun_binding_response(request: &[u8], src: SocketAddr) -> Option<Vec<u8>> {
    if !is_stun_binding_request(request) {
        return None;
    }
    let trans_id = &request[8..20];
    let mut resp = Vec::with_capacity(32);
    // STUN Header (20 байт): Type (0x0101), Length (12 байт attr), Magic Cookie, Trans ID
    resp.extend_from_slice(&[0x01, 0x01]);
    resp.extend_from_slice(&[0x00, 0x0c]);
    resp.extend_from_slice(&[0x21, 0x12, 0xa4, 0x42]);
    resp.extend_from_slice(trans_id);

    // XOR-MAPPED-ADDRESS (Type: 0x0020)
    match src {
        SocketAddr::V4(v4) => {
            resp.extend_from_slice(&[0x00, 0x20]); // Type
            resp.extend_from_slice(&[0x00, 0x08]); // Length = 8
            resp.push(0x00); // Reserved
            resp.push(0x01); // Family IPv4
            let xor_port = v4.port() ^ 0x2112;
            resp.extend_from_slice(&xor_port.to_be_bytes());
            let ip_u32 = u32::from(*v4.ip());
            let xor_ip = ip_u32 ^ 0x2112_a442;
            resp.extend_from_slice(&xor_ip.to_be_bytes());
        }
        SocketAddr::V6(v6) => {
            resp[2] = 0x00;
            resp[3] = 0x18; // Length = 24
            resp.extend_from_slice(&[0x00, 0x20]);
            resp.extend_from_slice(&[0x00, 0x14]);
            resp.push(0x00);
            resp.push(0x02); // Family IPv6
            let xor_port = v6.port() ^ 0x2112;
            resp.extend_from_slice(&xor_port.to_be_bytes());
            let ip_octets = v6.ip().octets();
            let mut cookie_trans = [0u8; 16];
            cookie_trans[..4].copy_from_slice(&[0x21, 0x12, 0xa4, 0x42]);
            cookie_trans[4..16].copy_from_slice(trans_id);
            for i in 0..16 {
                resp.push(ip_octets[i] ^ cookie_trans[i]);
            }
        }
    }
    Some(resp)
}

/// Извлекает SSRC из заголовка RTP-пакета (RFC 3550 §5.1).
pub fn parse_rtp_ssrc(data: &[u8]) -> Option<u32> {
    if data.len() >= 12 && (data[0] >> 6) == 2 {
        Some(u32::from_be_bytes([data[8], data[9], data[10], data[11]]))
    } else {
        None
    }
}

/// Создаёт RTCP PLI пакет (Picture Loss Indication, RFC 4585 §6.3.1) для запроса ключевого кадра.
pub fn build_rtcp_pli(sender_ssrc: u32, media_ssrc: u32) -> [u8; 12] {
    let mut pkt = [0u8; 12];
    pkt[0] = 0x81; // V=2, P=0, FMT=1 (PLI)
    pkt[1] = 206; // PT=206 (PSFB / Payload-Specific Feedback)
    pkt[2] = 0x00;
    pkt[3] = 0x02; // Length: 2 32-bit words following header
    pkt[4..8].copy_from_slice(&sender_ssrc.to_be_bytes());
    pkt[8..12].copy_from_slice(&media_ssrc.to_be_bytes());
    pkt
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;
    use tokio::time::{sleep, Duration};

    #[tokio::test]
    async fn udp_media_loop_send_and_receive() {
        let server = UdpMediaLoop::bind("127.0.0.1:0").await.expect("bind server");
        let client = UdpMediaLoop::bind("127.0.0.1:0").await.expect("bind client");

        let server_addr = server.local_addr();
        let client_addr = client.local_addr();

        let received = Arc::new(AtomicBool::new(false));
        let received_clone = received.clone();

        let _handle = server.start_receive_loop(move |src, packet| {
            assert_eq!(src, client_addr);
            assert_eq!(&packet.payload[..], &[10, 20, 30, 40]);
            received_clone.store(true, Ordering::SeqCst);
        });

        let test_pkt = RtpPacket::new(TrackId::new(), vec![10, 20, 30, 40]);
        client.send_packet(server_addr, &test_pkt).await.expect("send_packet");

        sleep(Duration::from_millis(50)).await;
        assert!(received.load(Ordering::SeqCst));

        assert_eq!(client.stats().packets_sent.load(Ordering::Relaxed), 1);
        assert_eq!(server.stats().packets_received.load(Ordering::Relaxed), 1);

        server.shutdown();
    }

    #[test]
    fn test_stun_binding_detection_and_response() {
        let mut req = vec![
            0x00, 0x01, // Binding Request
            0x00, 0x00, // Length = 0
            0x21, 0x12, 0xa4, 0x42, // Magic cookie
        ];
        req.extend_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12]); // Trans ID (12 bytes)
        assert!(is_stun_binding_request(&req));

        let client_addr: SocketAddr = "192.0.2.1:12345".parse().unwrap();
        let resp = build_stun_binding_response(&req, client_addr).expect("build response");
        assert_eq!(&resp[0..2], &[0x01, 0x01]); // Success response
        assert_eq!(&resp[4..8], &[0x21, 0x12, 0xa4, 0x42]); // Magic cookie preserved
        assert_eq!(&resp[8..20], &req[8..20]); // Trans ID preserved
        assert_eq!(&resp[20..22], &[0x00, 0x20]); // XOR-MAPPED-ADDRESS
    }

    #[test]
    fn test_rtp_ssrc_and_rtcp_pli() {
        let mut rtp = vec![0x80, 0x60, 0x00, 0x01, 0x00, 0x00, 0x01, 0x00];
        rtp.extend_from_slice(&0x12345678u32.to_be_bytes());
        rtp.extend_from_slice(&[1, 2, 3, 4]); // payload
        assert_eq!(parse_rtp_ssrc(&rtp), Some(0x12345678));

        let pli = build_rtcp_pli(0x00000001, 0x12345678);
        assert_eq!(pli[0], 0x81);
        assert_eq!(pli[1], 206);
        assert_eq!(&pli[4..8], &0x00000001u32.to_be_bytes());
        assert_eq!(&pli[8..12], &0x12345678u32.to_be_bytes());
    }

    #[tokio::test]
    async fn test_udp_media_loop_routing() {
        let server = UdpMediaLoop::bind("127.0.0.1:0").await.unwrap();
        let publisher = UdpMediaLoop::bind("127.0.0.1:0").await.unwrap();
        let subscriber = UdpMediaLoop::bind("127.0.0.1:0").await.unwrap();

        let sub_addr = subscriber.local_addr();
        let server_addr = server.local_addr();

        let sub_received = Arc::new(AtomicBool::new(false));
        let sub_received_clone = sub_received.clone();

        let _sub_task = subscriber.start_receive_loop(move |_src, packet| {
            if packet.payload[..] == [99, 88, 77] {
                sub_received_clone.store(true, Ordering::SeqCst);
            }
        });

        let _srv_task = server.start_routing_loop(move |_src, packet| {
            // Маршрутизируем пакет на адрес подписчика
            vec![(sub_addr, packet.payload)]
        });

        let pkt = RtpPacket::new(TrackId::new(), vec![99, 88, 77]);
        publisher.send_packet(server_addr, &pkt).await.unwrap();

        sleep(Duration::from_millis(50)).await;
        assert!(sub_received.load(Ordering::SeqCst));

        server.shutdown();
        publisher.shutdown();
        subscriber.shutdown();
    }

    #[test]
    fn test_parse_stun_username_and_authentication() {
        let username_str = "server-ufrag:client-ufrag";
        let user_bytes = username_str.as_bytes();
        let attr_len = user_bytes.len() as u16;
        let padded_len = (attr_len + 3) & !3;

        let mut req = vec![
            0x00, 0x01, // Binding Request
            0x00, 0x00, // Length placeholder (at 2..4)
            0x21, 0x12, 0xa4, 0x42, // Magic cookie
        ];
        req.extend_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12]); // Trans ID (12 bytes)

        // Attribute: USERNAME (0x0006)
        req.extend_from_slice(&0x0006u16.to_be_bytes());
        req.extend_from_slice(&attr_len.to_be_bytes());
        req.extend_from_slice(user_bytes);
        // Padding
        for _ in 0..(padded_len - attr_len) {
            req.push(0);
        }

        // Set message length in header
        let msg_len = (4 + padded_len) as u16;
        req[2..4].copy_from_slice(&msg_len.to_be_bytes());

        let parsed = parse_stun_username(&req);
        assert_eq!(parsed, Some("server-ufrag:client-ufrag"));

        assert!(is_stun_request_authenticated(&req, "server-ufrag"));
        assert!(!is_stun_request_authenticated(&req, "wrong-ufrag"));

        // Test truncated STUN packet
        assert_eq!(parse_stun_username(&req[..15]), None);
        assert_eq!(parse_stun_username(&req[..22]), None);
    }

    #[tokio::test]
    async fn test_udp_media_loop_drops_unauthenticated_stun() {
        let server = UdpMediaLoop::bind("127.0.0.1:0").await.unwrap();
        let client = UdpMediaLoop::bind("127.0.0.1:0").await.unwrap();
        let server_addr = server.local_addr();

        let _srv_task = server.start_routing_loop(|_src, _packet| Vec::new());

        // Unauthenticated STUN request (without USERNAME attribute)
        let unauthed_stun = vec![
            0x00, 0x01, // Binding Request
            0x00, 0x00, // Length = 0 (no attributes)
            0x21, 0x12, 0xa4, 0x42, // Magic cookie
            1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, // Trans ID
        ];

        client.send_raw(server_addr, &unauthed_stun).await.unwrap();
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

        assert_eq!(server.stats().packets_dropped.load(Ordering::Relaxed), 1);
        server.shutdown();
        client.shutdown();
    }
}
