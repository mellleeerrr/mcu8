//! DTLS-SRTP session state (MEDIA_PIPELINE §2.7).
//!
//! Тонкая testable state-модель DTLS-сессии + SRTP keying material.
//! Реальный DTLS-handshake (str0m) — в фазе3; здесь — только состояние
//! (Handshaking → Active → Closed) и SRTP keying placeholder.
//!
//! Feature `str0m` добавляет опциональный re-export str0m'а (для
//! фазы3 — реального handshakes). Без `str0m` крейт компилируется.

/// Конфигурация DTLS-сессии (MEDIA_PIPELINE §2.7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DtlsConfig {
    /// AES-128-CM (SRTP) / HMAC-SHA1 80-bit (SRTCP).
    pub cipher_suite: CipherSuite,
    /// PFS: ECDHE ephemeral.
    pub pfs: bool,
    /// SFU/MCU = ICE-lite = DTLS server (WebRTC client = DTLS client).
    pub role: DtlsRole,
}

impl Default for DtlsConfig {
    fn default() -> Self {
        Self { cipher_suite: CipherSuite::Aes128CmHmacSha1_80, pfs: true, role: DtlsRole::Server }
    }
}

/// SRTP cipher suite.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CipherSuite {
    /// AES-128-CM + HMAC-SHA1 80-bit (default).
    Aes128CmHmacSha1_80,
}

/// DTLS role.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DtlsRole {
    /// SFU/MCU (DTLS server).
    Server,
    /// WebRTC client (DTLS client).
    Client,
}

/// SRTP keying material (placeholder — реальная генерация в фазе3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SrtpKeyingMaterial {
    /// SRTP master key (128-bit).
    pub srtp_key: [u8; 16],
    /// SRTCP master key (128-bit).
    pub srtcp_key: [u8; 16],
    /// SRTP salt (96-bit).
    pub srtp_salt: [u8; 12],
    /// SRTCP salt (96-bit).
    pub srtcp_salt: [u8; 12],
}

/// Состояние DTLS-сессии.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DtlsState {
    /// Handshake in progress.
    Handshaking,
    /// Active (SRTP keying material готов).
    Active,
    /// Closed.
    Closed,
}

/// DTLS-сессия (thin state model).
#[derive(Debug, Clone)]
pub struct DtlsSession {
    config: DtlsConfig,
    state: DtlsState,
    keying: Option<SrtpKeyingMaterial>,
}

impl DtlsSession {
    /// Создаёт сессию в `Handshaking` (SRTP keying ещё нет).
    pub fn new(config: DtlsConfig) -> Self {
        Self { config, state: DtlsState::Handshaking, keying: None }
    }

    /// Создаёт active сессию (keying material задан).
    pub fn active(config: DtlsConfig, keying: SrtpKeyingMaterial) -> Self {
        Self { config, state: DtlsState::Active, keying: Some(keying) }
    }

    /// Конфигурация.
    pub fn config(&self) -> &DtlsConfig {
        &self.config
    }

    /// Текущее состояние.
    pub fn state(&self) -> DtlsState {
        self.state
    }

    /// SRTP keying material (None пока Handshaking).
    pub fn keying(&self) -> Option<&SrtpKeyingMaterial> {
        self.keying.as_ref()
    }

    /// Завершает handshake: Handshaking → Active (keying задан).
    pub fn complete_handshake(&mut self, keying: SrtpKeyingMaterial) {
        if self.state == DtlsState::Handshaking {
            self.state = DtlsState::Active;
            self.keying = Some(keying);
        }
    }

    /// Закрывает сессию (Active → Closed).
    pub fn close(&mut self) {
        self.state = DtlsState::Closed;
        self.keying = None;
    }

    /// Активна ли (SRTP доступен).
    pub fn is_active(&self) -> bool {
        self.state == DtlsState::Active
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_handshaking_no_keying() {
        let s = DtlsSession::new(DtlsConfig::default());
        assert_eq!(s.state(), DtlsState::Handshaking);
        assert!(s.keying().is_none());
        assert!(!s.is_active());
    }

    #[test]
    fn complete_handshake_makes_active() {
        let mut s = DtlsSession::new(DtlsConfig::default());
        let keying = SrtpKeyingMaterial { srtp_key: [1u8; 16], ..Default::default() };
        s.complete_handshake(keying);
        assert_eq!(s.state(), DtlsState::Active);
        assert!(s.is_active());
        assert_eq!(s.keying(), Some(&keying));
    }

    #[test]
    fn complete_handshake_is_idempotent_when_active() {
        let mut s = DtlsSession::new(DtlsConfig::default());
        s.complete_handshake(SrtpKeyingMaterial::default());
        let k2 = SrtpKeyingMaterial { srtp_key: [9u8; 16], ..Default::default() };
        s.complete_handshake(k2);
        // Active уже — keying не перезаписывается.
        assert_eq!(s.keying().unwrap().srtp_key, [0u8; 16]);
    }

    #[test]
    fn close_clears_keying() {
        let mut s = DtlsSession::new(DtlsConfig::default());
        s.complete_handshake(SrtpKeyingMaterial::default());
        s.close();
        assert_eq!(s.state(), DtlsState::Closed);
        assert!(s.keying().is_none());
        assert!(!s.is_active());
    }

    #[test]
    fn default_config_is_server_pfs() {
        let c = DtlsConfig::default();
        assert_eq!(c.role, DtlsRole::Server);
        assert!(c.pfs);
    }
}
