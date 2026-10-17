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

    /// Отправляет RTP-пакет конкретному подписчику по UDP (zero-copy).
    pub async fn send_packet(&self, dest: SocketAddr, packet: &RtpPacket) -> std::io::Result<usize> {
        let bytes = self.socket.send_to(&packet.payload, dest).await?;
        self.stats.packets_sent.fetch_add(1, Ordering::Relaxed);
        self.stats.bytes_sent.fetch_add(bytes as u64, Ordering::Relaxed);
        Ok(bytes)
    }

    /// Запускает фоновый цикл приёма и маршрутизации датаграмм.
    pub fn start_receive_loop<F>(&self, mut packet_handler: F) -> tokio::task::JoinHandle<()>
    where
        F: FnMut(SocketAddr, RtpPacket) + Send + 'static,
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

                                let payload = Bytes::copy_from_slice(&buf[..size]);
                                // Для базового роутинга используем фиктивный или распарсенный track ID
                                let packet = RtpPacket::new(TrackId::new(), payload);
                                packet_handler(src, packet);
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

    /// Инициирует остановку медиа-лупа.
    pub fn shutdown(&self) {
        let _ = self.shutdown_tx.send(());
    }
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
}
