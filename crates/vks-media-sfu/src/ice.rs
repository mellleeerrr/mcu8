//! ICE-lite (RFC 8445 §2; MEDIA_PIPELINE §2.8).
//!
//! SFU/MCU = **ICE-lite**: host candidates only (без srflx/relay).
//! Client — full ICE (host + srflx + relay). TURN — задача client.
//!
//! Здесь — `HostCandidate` + `IceLite` с mockable local-addr provider.
//! Без реальных сокетов: provider возвращает (host, port, transport).

use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6};

/// Transport (RFC 8445: UDP).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    /// UDP (default WebRTC).
    Udp,
}

/// Host candidate (ICE-lite).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostCandidate {
    /// Host address.
    pub addr: SocketAddr,
    /// Transport.
    pub transport: Transport,
}

impl HostCandidate {
    /// Строка `mDNS`-подобного `candidate` (для SDP, фаза3).
    pub fn to_sdp_fragment(&self) -> String {
        let transport = match self.transport {
            Transport::Udp => "UDP",
        };
        // candidate:0 1 UDP 2130706431 <ip> <port> typ host
        let priority: u32 = 100_000_000;
        format!("candidate:0 1 {transport} {priority} {} {} typ host", self.addr.ip(), self.addr.port())
    }
}

/// Local-addr provider (mockable).
pub trait LocalAddrProvider: Send + Sync {
    /// Возвращает локальные host-адреса (host candidates).
    fn local_addrs(&self) -> Vec<SocketAddr>;
}

/// In-memory provider (mock).
#[derive(Debug, Clone, Default)]
pub struct StaticAddrs {
    addrs: Vec<SocketAddr>,
}

impl StaticAddrs {
    /// Создаёт provider с фиксированным набором адресов.
    pub fn new(addrs: impl IntoIterator<Item = SocketAddr>) -> Self {
        Self { addrs: addrs.into_iter().collect() }
    }
}

impl LocalAddrProvider for StaticAddrs {
    fn local_addrs(&self) -> Vec<SocketAddr> {
        self.addrs.clone()
    }
}

/// Default provider: 127.0.0.1 (loopback) + ::1 (loopback IPv6).
#[derive(Debug, Clone, Copy, Default)]
pub struct LoopbackProvider;

impl LocalAddrProvider for LoopbackProvider {
    fn local_addrs(&self) -> Vec<SocketAddr> {
        vec![
            SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0)),
            SocketAddr::V6(SocketAddrV6::new(Ipv6Addr::LOCALHOST, 0, 0, 0)),
        ]
    }
}

/// ICE-lite: собирает host candidates (без srflx/relay).
pub struct IceLite {
    provider: Box<dyn LocalAddrProvider>,
}

impl std::fmt::Debug for IceLite {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IceLite").finish_non_exhaustive()
    }
}

impl IceLite {
    /// Создаёт ICE-lite с provider.
    pub fn new(provider: Box<dyn LocalAddrProvider>) -> Self {
        Self { provider }
    }

    /// Создаёт с loopback provider.
    pub fn loopback() -> Self {
        Self::new(Box::new(LoopbackProvider))
    }

    /// Собирает host candidates (ICE-lite: только host).
    pub fn gather(&self) -> Vec<HostCandidate> {
        self.provider.local_addrs().into_iter().map(|addr| HostCandidate { addr, transport: Transport::Udp }).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_provider_gives_host_candidates() {
        let ice = IceLite::loopback();
        let cands = ice.gather();
        assert_eq!(cands.len(), 2);
        assert!(cands.iter().all(|c| c.transport == Transport::Udp));
        // Первый — IPv4 loopback.
        assert_eq!(cands[0].addr, SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0)));
    }

    #[test]
    fn static_provider_custom_addrs() {
        let ip: std::net::Ipv4Addr = "129.0.0.1".parse().unwrap();
        let addrs = StaticAddrs::new(vec![SocketAddr::V4(SocketAddrV4::new(ip, 0))]);
        let ice = IceLite::new(Box::new(addrs));
        let cands = ice.gather();
        assert_eq!(cands.len(), 1);
        assert_eq!(cands[0].addr, SocketAddr::V4(SocketAddrV4::new(ip, 0)));
    }

    #[test]
    fn empty_provider_gives_no_candidates() {
        let addrs = StaticAddrs::new(Vec::<SocketAddr>::new());
        let ice = IceLite::new(Box::new(addrs));
        assert!(ice.gather().is_empty());
    }

    #[test]
    fn sdp_fragment_format() {
        let ip: Ipv4Addr = "192.168.1.1".parse().unwrap();
        let c = HostCandidate { addr: SocketAddr::V4(SocketAddrV4::new(ip, 50000)), transport: Transport::Udp };
        let sdp = c.to_sdp_fragment();
        assert!(sdp.starts_with("candidate:"));
        assert!(sdp.contains("192.168.1.1"));
        assert!(sdp.contains("50000"));
        assert!(sdp.contains("typ host"));
    }
}
