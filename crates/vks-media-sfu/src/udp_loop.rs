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
use tracing::{debug, info, trace, warn};
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
    /// Входящие RTCP-пакеты, отделённые от RTP (не маршрутизируются как медиа).
    pub packets_rtcp: AtomicU64,
}

/// Классификация входящей UDP-датаграммы по RFC 7983 §7 (мультиплексирование
/// RTP/RTCP/STUN/DTLS на одном порту).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum IncomingPacket {
    /// STUN (в т.ч. ICE Binding Request): первый байт 0..=3 + magic cookie.
    Stun,
    /// DTLS: первый байт 20..=63 (RFC 7983).
    Dtls,
    /// RTCP: version=2, second octet (PT, 8 бит) в 192..=223.
    Rtcp,
    /// RTP: version=2 и второй октет не попадает в диапазон RTCP PT.
    Rtp,
    /// Всё остальное (ZRTP, TURN Channel, битые/чужие протоколы).
    Unknown,
}

/// Magic cookie STUN (RFC 5389 §6).
const STUN_MAGIC_COOKIE: [u8; 4] = [0x21, 0x12, 0xa4, 0x42];
/// Минимальный размер фиксированного заголовка RTP (RFC 3550 §5.1);
/// короче — это уже не RTP, а обрезанная/чужая датаграмма.
const RTP_FIXED_HEADER_LEN: usize = 12;
/// Минимальный корректный RTCP-пакет: заголовок (4) + SSRC первого отчёта (4)
/// (RFC 3550 §6.4.1); короче — не RTCP.
const RTCP_MIN_LEN: usize = 8;

/// Определяет протокол входящей датагрампы по первым байтам (RFC 7983 §7).
///
/// Функция не паникует на коротких пакетах и не выдаёт `Rtp`/`Rtcp` за
/// датаграммы, которые физически не могут быть этим протоколом: `Rtp`
/// требует полного фиксированного заголовка (12 байт), `Rtcp` — минимум
/// заголовка с SSRC (8 байт). Всё недоказанное — `Unknown` (отбросить).
///
/// Контракт: `IncomingPacket::Rtp` ⇒ `len >= 12 && version == 2 &&`
/// `второй октет ∉ 192..=223`; `IncomingPacket::Rtcp` ⇒ `len >= 8 &&`
/// `version == 2 && второй октет ∈ 192..=223`.
pub(crate) fn classify_incoming(data: &[u8]) -> IncomingPacket {
    if data.len() < 2 {
        return IncomingPacket::Unknown;
    }

    // 1. STUN: RFC 7983 отводит под STUN первый байт 0..=3, а magic cookie
    //    отсекает мусор. Ловим все STUN-сообщения (Binding Request/Success/
    //    Error/Indication), а не только Binding Request.
    if data[0] <= 3 && data.len() >= 8 && data[4..8] == STUN_MAGIC_COOKIE {
        return IncomingPacket::Stun;
    }

    // 2. DTLS: Handshake (22), Alert (21), ChangeCipherSpec (20), AppData (23).
    if (20..=63).contains(&data[0]) {
        return IncomingPacket::Dtls;
    }

    // 3. RTP / RTCP: version=2 (старшие два бита первого байта == 0b10).
    if data[0] >> 6 == 2 {
        // У RTCP поле PT занимает все 8 бит (192..=223), у RTP — только 7
        // бит (0..=127), а старший бит второго октета — M (marker).
        // Поэтому RTCP отличается по полному второму октету.
        if (192..=223).contains(&data[1]) {
            return if data.len() >= RTCP_MIN_LEN { IncomingPacket::Rtcp } else { IncomingPacket::Unknown };
        }
        return if data.len() >= RTP_FIXED_HEADER_LEN { IncomingPacket::Rtp } else { IncomingPacket::Unknown };
    }

    IncomingPacket::Unknown
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

                                if size == 0 {
                                    continue;
                                }

                                match classify_incoming(&buf[..size]) {
                                    IncomingPacket::Stun => {
                                        // 1. Автоматический ответ на STUN Binding Request (RFC 5389 / RFC 8445 ICE-lite)
                                        if is_stun_binding_request(&buf[..size]) {
                                            if let Some(username) = parse_stun_username(&buf[..size]) {
                                                if !username.trim().is_empty() {
                                                    if let Some(resp) = build_stun_binding_response(&buf[..size], src) {
                                                        let _ = socket.send_to(&resp, src).await;
                                                        continue;
                                                    }
                                                }
                                            }
                                        }
                                        stats.packets_dropped.fetch_add(1, Ordering::Relaxed);
                                    }
                                    IncomingPacket::Dtls => {
                                        // 2. DTLS packets (RFC 7983 §7): Handshake (22), Alert (21), AppData (23)
                                        stats.packets_dropped.fetch_add(1, Ordering::Relaxed);
                                    }
                                    IncomingPacket::Rtcp => {
                                        // 3. RTCP НЕ является медиа-пакетом: никогда не отдаём его
                                        //    router'у как RtpPacket (иначе, например, PLI уйдёт
                                        //    подписчикам как RTP с чужим SSRC). Учёт — отдельный.
                                        stats.packets_rtcp.fetch_add(1, Ordering::Relaxed);
                                        // pt — тип RTCP-пакета (200 SR, 201 RR, 205 RTPFB/NACK,
                                        // 206 PSFB/PLI/FIR…), fmt — подтип (FMT) для диагностики.
                                        trace!(%src, size, pt = buf[1], fmt = buf[0] & 0x1f, "incoming RTCP packet dropped (not routed as media)");
                                    }
                                    IncomingPacket::Unknown => {
                                        // 4. Неизвестный протокол / мусор.
                                        stats.packets_dropped.fetch_add(1, Ordering::Relaxed);
                                    }
                                    IncomingPacket::Rtp => {
                                        // 5. RTP (RFC 3550 / RFC 7983): маршрутизация подписчикам.
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
///
/// Возвращает `None` для не-RTP датаграмм, в том числе для RTCP: у RTCP поле
/// PT занимает все 8 бит второго октета (192..=223), поэтому байты 8..12 —
/// это SSRC получателя/медиа, а не SSRC отправителя медиа-потока. Вызывать
/// эту функцию на RTCP нельзя (см. `classify_incoming`).
pub fn parse_rtp_ssrc(data: &[u8]) -> Option<u32> {
    // Ранние отказы: не-RTP version и RTCP (PT 192..=223) не должны давать SSRC.
    if data.len() < 12 || (data[0] >> 6) != 2 || (192..=223).contains(&data[1]) {
        return None;
    }
    Some(u32::from_be_bytes([data[8], data[9], data[10], data[11]]))
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
    use std::sync::atomic::{AtomicBool, AtomicUsize};
    use tokio::time::{sleep, Duration};

    /// Собирает валидный RTP-пакет: фиксированный заголовок (12 байт) + payload.
    fn rtp_packet(ssrc: u32, sequence: u16, payload_type: u8, marker: bool, payload: &[u8]) -> Vec<u8> {
        let mut packet = Vec::with_capacity(RTP_FIXED_HEADER_LEN + payload.len());
        packet.push(0x80); // V=2, P=0, X=0, CC=0
        packet.push(if marker { payload_type | 0x80 } else { payload_type });
        packet.extend_from_slice(&sequence.to_be_bytes());
        packet.extend_from_slice(&0u32.to_be_bytes()); // timestamp
        packet.extend_from_slice(&ssrc.to_be_bytes());
        packet.extend_from_slice(payload);
        packet
    }

    /// Собирает валидный RTCP-пакет (заголовок 4 байта + SSRC, минимум 8 байт).
    fn rtcp_packet(payload_type: u8, fmt: u8, ssrc: u32) -> Vec<u8> {
        let mut packet = Vec::with_capacity(RTCP_MIN_LEN);
        packet.push(0x80 | (fmt & 0x1f));
        packet.push(payload_type);
        packet.extend_from_slice(&1u16.to_be_bytes()); // length = 1 → 8 байт
        packet.extend_from_slice(&ssrc.to_be_bytes());
        packet
    }

    #[tokio::test]
    async fn udp_media_loop_send_and_receive() {
        let server = UdpMediaLoop::bind("127.0.0.1:0").await.expect("bind server");
        let client = UdpMediaLoop::bind("127.0.0.1:0").await.expect("bind client");

        let server_addr = server.local_addr();
        let client_addr = client.local_addr();

        // Минимальный валидный RTP-заголовок: медиа-луп классифицирует датаграммы
        // по RFC 7983 и не пропускает в router() пакеты других протоколов.
        let test_payload = rtp_packet(0xdead_beef, 1, 96, false, &[10, 20, 30, 40]);

        let received = Arc::new(AtomicBool::new(false));
        let received_clone = received.clone();
        let expected_payload = test_payload.clone();

        let _handle = server.start_receive_loop(move |src, packet| {
            assert_eq!(src, client_addr);
            assert_eq!(&packet.payload[..], &expected_payload[..]);
            received_clone.store(true, Ordering::SeqCst);
        });

        let test_pkt = RtpPacket::new(TrackId::new(), test_payload);
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

    #[test]
    fn test_classify_incoming_protocols() {
        // ---------- STUN: ловим все типы сообщений, не только Binding Request ----------
        let mut stun_request = vec![
            0x00, 0x01, // Binding Request
            0x00, 0x00, // Length = 0
            0x21, 0x12, 0xa4, 0x42, // Magic cookie
        ];
        stun_request.extend_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12]); // Trans ID
        assert_eq!(classify_incoming(&stun_request), IncomingPacket::Stun);

        let mut stun_success = stun_request.clone();
        stun_success[0] = 0x01; // Binding Success 0x0101
        assert_eq!(classify_incoming(&stun_success), IncomingPacket::Stun);

        let mut stun_indication = stun_request.clone();
        stun_indication[1] = 0x11; // Send Indication 0x0011
        assert_eq!(classify_incoming(&stun_indication), IncomingPacket::Stun);

        // STUN-подобный первый байт без magic cookie — не STUN.
        assert_eq!(classify_incoming(&[0x00, 0x01, 0x00, 0x00, 0xde, 0xad, 0xbe, 0xef]), IncomingPacket::Unknown);

        // ---------- DTLS (RFC 7983: первый байт 20..=63) ----------
        for content_type in [20u8, 21, 22, 23, 63] {
            let mut dtls = vec![content_type, 0xfe, 0xfd, 0x00, 0x00, 0x00, 0x00, 0x00];
            assert_eq!(classify_incoming(&dtls), IncomingPacket::Dtls, "DTLS content_type={content_type}");
            dtls[0] = 19; // ZRTP — уже не DTLS
            assert_eq!(classify_incoming(&dtls), IncomingPacket::Unknown);
        }
        assert_eq!(classify_incoming(&[64, 0x00, 0x00, 0x00]), IncomingPacket::Unknown); // TURN channel

        // ---------- RTCP: PT 192..=223, padding, compound, границы ----------
        for pt in 192u8..=223 {
            assert_eq!(classify_incoming(&rtcp_packet(pt, 0, 0x0000_0001)), IncomingPacket::Rtcp, "RTCP PT={pt}");
        }
        // P-бит (padding, 0x20) не мешает классификации.
        let mut padded_sr = rtcp_packet(200, 0, 1);
        padded_sr[0] |= 0x20;
        padded_sr.extend_from_slice(&[0, 0, 0, 2]);
        assert_eq!(classify_incoming(&padded_sr), IncomingPacket::Rtcp);
        // Compound RTCP: SR + SDES — классифицируется по первому пакету.
        let mut compound = rtcp_packet(200, 5, 0xaaaa_bbbb);
        compound.extend_from_slice(&rtcp_packet(202, 1, 0xaaaa_bbbb));
        assert_eq!(classify_incoming(&compound), IncomingPacket::Rtcp);
        // PLI из продакшн-билдера.
        assert_eq!(classify_incoming(&build_rtcp_pli(1, 2)), IncomingPacket::Rtcp);
        // Граница второго октета: 191 — RTP, 192/223 — RTCP, 224 — RTP (marker).
        assert_eq!(classify_incoming(&rtp_packet(1, 1, 63, true, &[])), IncomingPacket::Rtp); // 0xBF
        assert_eq!(classify_incoming(&rtp_packet(1, 1, 64, true, &[])), IncomingPacket::Rtcp); // 0xC0
        assert_eq!(classify_incoming(&rtp_packet(1, 1, 95, true, &[])), IncomingPacket::Rtcp); // 0xDF
        assert_eq!(classify_incoming(&rtp_packet(1, 1, 96, true, &[])), IncomingPacket::Rtp); // 0xE0

        // Неполный RTCP-заголовок — не RTCP; минимальный RTCP (RR, 8 байт) — RTCP.
        assert_eq!(classify_incoming(&[0x81, 206]), IncomingPacket::Unknown);
        assert_eq!(classify_incoming(&[0x81, 206, 0x00, 0x02]), IncomingPacket::Unknown);
        assert_eq!(classify_incoming(&rtcp_packet(201, 0, 7)), IncomingPacket::Rtcp);

        // ---------- RTP: version=2, 7-битный PT, marker, padding, длина ----------
        assert_eq!(classify_incoming(&rtp_packet(0x1122_3344, 1, 96, false, &[1, 2, 3])), IncomingPacket::Rtp);
        assert_eq!(classify_incoming(&rtp_packet(0x1122_3344, 1, 96, true, &[1, 2, 3])), IncomingPacket::Rtp);
        assert_eq!(classify_incoming(&rtp_packet(1, 1, 0, true, &[])), IncomingPacket::Rtp); // 0x80
        assert_eq!(classify_incoming(&rtp_packet(1, 1, 127, true, &[])), IncomingPacket::Rtp); // 0xFF
        let mut padded_rtp = rtp_packet(1, 1, 96, false, &[0, 0, 0, 4]);
        padded_rtp[0] |= 0x20; // P=1
        assert_eq!(classify_incoming(&padded_rtp), IncomingPacket::Rtp);
        // RFC 5761 §4: PT 64..95 в rtcp-mux запрещены; при marker=1 второй октет
        // попадает в 192..=223, и такая нестандартная RTP-датаграмма трактуется как RTCP.
        assert_eq!(classify_incoming(&rtp_packet(1, 1, 72, true, &[])), IncomingPacket::Rtcp);
        // Ровно 12 байт — минимальный RTP; 11 байт — уже не RTP.
        assert_eq!(classify_incoming(&rtp_packet(1, 1, 96, false, &[])), IncomingPacket::Rtp);
        assert_eq!(classify_incoming(&rtp_packet(1, 1, 96, false, &[0])[..11]), IncomingPacket::Unknown);

        // ---------- Unknown: чужие версии и протоколы ----------
        assert_eq!(classify_incoming(&[0x40, 0x00, 0x00, 0x00]), IncomingPacket::Unknown); // version 1
        assert_eq!(classify_incoming(&[0x10, 0x00, 0x00, 0x00]), IncomingPacket::Unknown); // ZRTP 16..19
        assert_eq!(classify_incoming(&[0x50, 0x00, 0x00, 0x00]), IncomingPacket::Unknown); // 80..127
        assert_eq!(classify_incoming(&[0x00, 0x00, 0x00, 0x00]), IncomingPacket::Unknown); // version 0 без cookie

        // ---------- Короткие датаграммы: без паники и не «медиа» ----------
        assert_eq!(classify_incoming(&[]), IncomingPacket::Unknown);
        assert_eq!(classify_incoming(&[0x80]), IncomingPacket::Unknown);
        assert_eq!(classify_incoming(&[0x81, 206]), IncomingPacket::Unknown);
    }

    /// Исчерпывающая проверка контракта классификатора по первым двум байтам +
    /// детерминированный fuzz (без внешних зависимостей): классификатор не
    /// паникует и не может выдать RTP/RTCP для заведомо невалидных датаграмм.
    #[test]
    fn test_classify_incoming_invariants_exhaustive_and_fuzz() {
        let mut buf = [0u8; RTP_FIXED_HEADER_LEN];
        for b0 in 0u8..=255 {
            for b1 in 0u8..=255 {
                buf[0] = b0;
                buf[1] = b1;
                match classify_incoming(&buf) {
                    IncomingPacket::Rtcp => {
                        assert!(b0 >> 6 == 2 && (192..=223).contains(&b1));
                        // Инвариант demux↔движок: RTCP никогда не даёт RTP SSRC.
                        assert_eq!(parse_rtp_ssrc(&buf), None);
                    }
                    IncomingPacket::Rtp => {
                        assert!(b0 >> 6 == 2 && !(192..=223).contains(&b1));
                        // Инвариант demux↔движок: любой Rtp после классификации
                        // всегда даёт SSRC (движку не нужен fallback без SSRC).
                        assert!(parse_rtp_ssrc(&buf).is_some());
                    }
                    IncomingPacket::Dtls => assert!((20..=63).contains(&b0)),
                    // В buf cookie STUN нулевая, поэтому Stun здесь невозможен;
                    // отдельный кейс ниже.
                    IncomingPacket::Stun => panic!("STUN without magic cookie must not be classified as STUN"),
                    IncomingPacket::Unknown => {}
                }
                // Классификация зависит от префикса, а не от «хвоста» датаграммы.
                let mut longer = buf.to_vec();
                longer.extend_from_slice(&[0xAA; 64]);
                assert_eq!(classify_incoming(&longer), classify_incoming(&buf));
            }
        }
        // STUN с cookie — единственный способ получить Stun.
        for b0 in 0u8..=3 {
            let mut stun = vec![b0, 0x01, 0x00, 0x00, 0x21, 0x12, 0xa4, 0x42];
            stun.extend_from_slice(&[0u8; 12]);
            assert_eq!(classify_incoming(&stun), IncomingPacket::Stun);
            stun[7] ^= 0xFF; // ломаем cookie
            assert_ne!(classify_incoming(&stun), IncomingPacket::Stun);
        }

        // Детерминированный LCG-фузз: произвольные длины 0..=64.
        let mut state = 0x1234_5678_9abc_def0u64;
        for _ in 0..20_000 {
            state = state.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
            let len = (state >> 33) as usize % 65;
            let mut packet = vec![0u8; len];
            for byte in &mut packet {
                state = state.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
                *byte = (state >> 56) as u8;
            }
            match classify_incoming(&packet) {
                IncomingPacket::Rtp => {
                    assert!(
                        packet.len() >= RTP_FIXED_HEADER_LEN
                            && packet[0] >> 6 == 2
                            && !(192..=223).contains(&packet[1])
                    );
                    assert!(parse_rtp_ssrc(&packet).is_some(), "Rtp без SSRC сломал бы маршрутизацию");
                }
                IncomingPacket::Rtcp => {
                    assert!(packet.len() >= RTCP_MIN_LEN && packet[0] >> 6 == 2 && (192..=223).contains(&packet[1]));
                    assert_eq!(parse_rtp_ssrc(&packet), None, "RTCP не должен давать RTP SSRC");
                }
                IncomingPacket::Dtls => assert!((20..=63).contains(&packet[0])),
                IncomingPacket::Stun => {
                    assert!(packet.len() >= 8 && packet[0] <= 3 && packet[4..8] == STUN_MAGIC_COOKIE)
                }
                IncomingPacket::Unknown => {}
            }
        }
    }

    #[test]
    fn test_parse_rtp_ssrc_rejects_rtcp() {
        // RTCP PLI: байты 8..12 — media SSRC, а не SSRC отправителя RTP.
        let pli = build_rtcp_pli(0x0000_0001, 0x1234_5678);
        assert_eq!(parse_rtp_ssrc(&pli), None);

        // RTCP Sender Report — тоже не RTP.
        let sr = [0x80, 200, 0x00, 0x06, 0, 0, 0, 1, 0xaa, 0xbb, 0xcc, 0xdd];
        assert_eq!(parse_rtp_ssrc(&sr), None);

        // RTP по-прежнему даёт SSRC.
        let mut rtp = vec![0x80, 0x60, 0x00, 0x01, 0x00, 0x00, 0x01, 0x00];
        rtp.extend_from_slice(&0x12345678u32.to_be_bytes());
        rtp.extend_from_slice(&[1, 2, 3, 4]);
        assert_eq!(parse_rtp_ssrc(&rtp), Some(0x12345678));

        // Не-RTP version и короткие пакеты — None (без паники).
        assert_eq!(parse_rtp_ssrc(&[0x40, 0x60, 0, 0, 0, 0, 0, 0, 1, 2, 3, 4]), None);
        assert_eq!(parse_rtp_ssrc(&[0x80, 0x60]), None);
        assert_eq!(parse_rtp_ssrc(&[]), None);
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
        // Валидный RTP-заголовок (V=2, PT=96) — иначе классификатор отбросит пакет.
        let routed_payload = rtp_packet(0x0000_1234, 7, 96, false, &[99, 88, 77]);
        let expected_payload = routed_payload.clone();

        let _sub_task = subscriber.start_receive_loop(move |_src, packet| {
            if packet.payload[..] == expected_payload[..] {
                sub_received_clone.store(true, Ordering::SeqCst);
            }
        });

        let _srv_task = server.start_routing_loop(move |_src, packet| {
            // Маршрутизируем пакет на адрес подписчика
            vec![(sub_addr, packet.payload)]
        });

        let pkt = RtpPacket::new(TrackId::new(), routed_payload);
        publisher.send_packet(server_addr, &pkt).await.unwrap();

        // Ожидаем доставку с запасом: фиксированные 50 мс флейкуют на
        // загруженном CI-раннере (2 vCPU, параллельные тесты).
        tokio::time::timeout(Duration::from_secs(2), async {
            while !sub_received.load(Ordering::SeqCst) {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("routed RTP packet must reach subscriber");

        server.shutdown();
        publisher.shutdown();
        subscriber.shutdown();
    }

    #[tokio::test]
    async fn test_udp_media_loop_drops_incoming_rtcp() {
        let server = UdpMediaLoop::bind("127.0.0.1:0").await.unwrap();
        let publisher = UdpMediaLoop::bind("127.0.0.1:0").await.unwrap();

        let publisher_addr = publisher.local_addr();
        let server_addr = server.local_addr();

        // Счётчик вызовов router — RTCP не должен до него доходить.
        let routed = Arc::new(AtomicUsize::new(0));
        let routed_clone = routed.clone();

        let _srv_task = server.start_routing_loop(move |_src, packet| {
            routed_clone.fetch_add(1, Ordering::SeqCst);
            vec![(publisher_addr, packet.payload)]
        });

        // Входящий RTCP PLI: раньше классифицировался как «RTP / RTCP» и уходил
        // в router(), а SSRC из байт 8..12 (media SSRC) — подписчикам как медиа.
        let pli = build_rtcp_pli(0x0000_0001, 0x1234_5678);
        publisher.send_raw(server_addr, &pli).await.unwrap();
        sleep(Duration::from_millis(50)).await;

        assert_eq!(routed.load(Ordering::SeqCst), 0, "RTCP must never be routed as RTP");
        assert_eq!(server.stats().packets_rtcp.load(Ordering::Relaxed), 1);
        assert_eq!(server.stats().packets_dropped.load(Ordering::Relaxed), 0);

        // Луп продолжает работать: следующий RTP-пакет маршрутизируется.
        let rtp = [0x80, 96, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x11, 0x22, 0x33, 0x44];
        publisher.send_raw(server_addr, &rtp).await.unwrap();
        sleep(Duration::from_millis(50)).await;

        assert_eq!(routed.load(Ordering::SeqCst), 1, "RTP after RTCP must still be routed");
        assert_eq!(server.stats().packets_rtcp.load(Ordering::Relaxed), 1);

        server.shutdown();
        publisher.shutdown();
    }

    /// End-to-end через реальные UDP-сокеты и настоящий `SfuEngine`: несколько
    /// RTP SSRC + вперемешку RTCP всех типов (SR/RR/PLI/NACK/BYE/compound) —
    /// подписчик получает только RTP, в правильном порядке, без дублей/потерь.
    #[tokio::test]
    async fn test_udp_media_loop_multiple_ssrc_and_rtcp_do_not_mix() {
        use vks_core::ports::media_engine::{MediaEngine, RoomCaps, RoomMediaCfg, SubscribeRequest};
        use vks_core::track::{Track, TrackKind};
        use vks_core::{ParticipantId, RoomId, RoomMode};

        let engine = Arc::new(crate::engine::SfuEngine::new());
        let room = RoomId::new();
        engine
            .create_room(RoomMediaCfg { room_id: room, mode: RoomMode::Sfu, caps: RoomCaps::default() })
            .await
            .unwrap();

        let publisher_id = ParticipantId::new();
        let subscriber_id = ParticipantId::new();
        let video = Track::new(publisher_id, TrackKind::Video);
        let audio = Track::new(publisher_id, TrackKind::Audio);
        let (video_track, audio_track) = (video.id, audio.id);
        engine.add_publisher(room, publisher_id, vec![video, audio]).await.unwrap();
        engine
            .add_subscriber(room, subscriber_id, SubscribeRequest { track_ids: Vec::new(), audio_only: false })
            .await
            .unwrap();

        const VIDEO_SSRC: u32 = 0x1111_2222;
        const AUDIO_SSRC: u32 = 0x3333_4444;
        engine.register_ssrc(room, publisher_id, video_track, VIDEO_SSRC);
        engine.register_ssrc(room, publisher_id, audio_track, AUDIO_SSRC);

        let server = UdpMediaLoop::bind("127.0.0.1:0").await.unwrap();
        let publisher = UdpMediaLoop::bind("127.0.0.1:0").await.unwrap();
        let subscriber = UdpMediaLoop::bind("127.0.0.1:0").await.unwrap();
        engine.register_endpoint(room, subscriber_id, subscriber.local_addr());

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();
        let _sub_task = subscriber.start_receive_loop(move |_src, packet| {
            let _ = tx.send(packet.payload.to_vec());
        });

        let engine_clone = engine.clone();
        let server_addr = server.local_addr();
        let _srv_task =
            server.start_routing_loop(move |src, packet| engine_clone.route_udp_packet(src, packet.payload));

        let video_frame_1 = rtp_packet(VIDEO_SSRC, 1, 96, false, &[0x10, 0x00, 0x00, 0x01]);
        let audio_frame = rtp_packet(AUDIO_SSRC, 1, 111, false, &[0xf8, 0xff, 0xfe]);
        let video_frame_2 = rtp_packet(VIDEO_SSRC, 2, 96, true, &[0x20, 0x00, 0x00, 0x02]);

        // Порядок как в живом rtcp-mux потоке: RTP ↔ RTCP.
        let mut compound = rtcp_packet(200, 0, VIDEO_SSRC);
        compound.extend_from_slice(&rtcp_packet(202, 1, VIDEO_SSRC)); // compound: SR + SDES
        let datagrams: Vec<Vec<u8>> = vec![
            video_frame_1.clone(),
            rtcp_packet(200, 0, VIDEO_SSRC),        // SR
            rtcp_packet(201, 0, AUDIO_SSRC),        // RR
            build_rtcp_pli(0, VIDEO_SSRC).to_vec(), // PSFB / PLI (206)
            rtcp_packet(205, 1, VIDEO_SSRC),        // RTPFB / NACK
            rtcp_packet(203, 0, AUDIO_SSRC),        // BYE
            compound,                               // compound RTCP (SR + SDES)
            audio_frame.clone(),
            video_frame_2.clone(),
        ];

        let rtcp_datagrams = 6; // #2..#7 в списке выше
        for datagram in &datagrams {
            publisher.send_raw(server_addr, datagram).await.unwrap();
        }

        // Подписчик получает ровно 3 RTP-пакета в порядке отправки.
        for want in [&video_frame_1, &audio_frame, &video_frame_2] {
            let got = tokio::time::timeout(Duration::from_millis(500), rx.recv())
                .await
                .expect("subscriber must receive RTP packet")
                .expect("subscriber channel closed");
            assert_eq!(&got, want, "subscriber received a non-RTP or reordered datagram");
        }
        // …и ни одной RTCP-датаграммы сверх этого.
        assert!(
            tokio::time::timeout(Duration::from_millis(100), rx.recv()).await.is_err(),
            "RTCP must never reach subscribers as media"
        );

        let stats = server.stats();
        assert_eq!(stats.packets_received.load(Ordering::Relaxed), datagrams.len() as u64);
        assert_eq!(stats.packets_rtcp.load(Ordering::Relaxed), rtcp_datagrams);
        assert_eq!(stats.packets_dropped.load(Ordering::Relaxed), 0);
        assert_eq!(stats.packets_sent.load(Ordering::Relaxed), 3);
        // Symmetric RTP: эндпоинт паблишера выучен по первому RTP.
        assert_eq!(engine.get_endpoint(room, publisher_id), Some(publisher.local_addr()));

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
        req.resize(req.len() + (padded_len - attr_len) as usize, 0);

        // Set message length in header
        let msg_len = 4 + padded_len;
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
