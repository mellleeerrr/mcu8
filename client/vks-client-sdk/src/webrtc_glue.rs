//! WebRTC glue: обёртки над web-sys/WebRTC API (Q8: вариант (а)).
//!
//! Основание: `docs/rules/FRONTEND_RULES.md` §7.4 (Q8),
//! `docs/decisions/DECISIONS.md` (A8.1, A8.2).
//!
//! Варианты (Q8):
//! - (а) `web-sys` + WebRTC API браузера (RTCPeerConnection) — bundle < 500KB.
//! - (б) str0m in wasm32 — full stack, bundle ~1-2MB.
//!
//! Сейчас — абстракция `WebrtcPeer` + mock; impl (а) добавляется в фазе 8.

use async_trait::async_trait;
use thiserror::Error;

/// Ошибка WebRTC.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum WebRtcError {
    #[error("peer connection: {0}")]
    Peer(String),
    #[error("ice: {0}")]
    Ice(String),
    #[error("dtls: {0}")]
    Dtls(String),
    #[error("unsupported: {0}")]
    Unsupported(String),
}

/// WebRTC peer (абстракция над RTCPeerConnection / str0m).
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
pub trait WebrtcPeer: Send + Sync {
    /// Создаёт offer SDP.
    async fn create_offer(&self) -> Result<String, WebRtcError>;
    /// Устанавливает remote answer.
    async fn set_remote_answer(&self, sdp: &str) -> Result<(), WebRtcError>;
    /// Добавляет ICE candidate.
    async fn add_ice_candidate(&self, candidate: &str) -> Result<(), WebRtcError>;
    /// Завершает соединение.
    fn close(&self);
}

/// Заглушка (для тестов + до фазы 8 impl).
#[derive(Debug, Default)]
pub struct MockPeer;

#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
impl WebrtcPeer for MockPeer {
    async fn create_offer(&self) -> Result<String, WebRtcError> {
        Ok("v=0\nm=audio 9\nm=video 19".into())
    }
    async fn set_remote_answer(&self, _sdp: &str) -> Result<(), WebRtcError> {
        Ok(())
    }
    async fn add_ice_candidate(&self, _candidate: &str) -> Result<(), WebRtcError> {
        Ok(())
    }
    fn close(&self) {}
}

#[cfg(target_arch = "wasm32")]
pub struct WebSysPeer {
    inner: web_sys::RtcPeerConnection,
}

#[cfg(target_arch = "wasm32")]
// SAFETY: wasm32-unknown-unknown single-threaded JS environment
unsafe impl Send for WebSysPeer {}
#[cfg(target_arch = "wasm32")]
unsafe impl Sync for WebSysPeer {}

#[cfg(target_arch = "wasm32")]
impl WebSysPeer {
    pub fn new() -> Result<Self, WebRtcError> {
        let rtc_config = web_sys::RtcConfiguration::new();
        let pc = web_sys::RtcPeerConnection::new_with_configuration(&rtc_config)
            .map_err(|e| WebRtcError::Peer(format!("{e:?}")))?;
        Ok(Self { inner: pc })
    }

    pub fn with_config(config: &web_sys::RtcConfiguration) -> Result<Self, WebRtcError> {
        let pc = web_sys::RtcPeerConnection::new_with_configuration(config)
            .map_err(|e| WebRtcError::Peer(format!("{e:?}")))?;
        Ok(Self { inner: pc })
    }

    pub fn peer_connection(&self) -> &web_sys::RtcPeerConnection {
        &self.inner
    }
}

#[cfg(target_arch = "wasm32")]
#[async_trait(?Send)]
impl WebrtcPeer for WebSysPeer {
    async fn create_offer(&self) -> Result<String, WebRtcError> {
        let promise = self.inner.create_offer();
        let js_val =
            wasm_bindgen_futures::JsFuture::from(promise).await.map_err(|e| WebRtcError::Peer(format!("{e:?}")))?;

        let sdp = js_sys::Reflect::get(&js_val, &wasm_bindgen::JsValue::from_str("sdp"))
            .ok()
            .and_then(|v| v.as_string())
            .unwrap_or_default();

        let desc = web_sys::RtcSessionDescriptionInit::new(web_sys::RtcSdpType::Offer);
        desc.set_sdp(&sdp);

        let local_desc_promise = self.inner.set_local_description(&desc);
        wasm_bindgen_futures::JsFuture::from(local_desc_promise)
            .await
            .map_err(|e| WebRtcError::Peer(format!("{e:?}")))?;

        Ok(sdp)
    }

    async fn set_remote_answer(&self, sdp: &str) -> Result<(), WebRtcError> {
        let desc = web_sys::RtcSessionDescriptionInit::new(web_sys::RtcSdpType::Answer);
        desc.set_sdp(sdp);
        let promise = self.inner.set_remote_description(&desc);
        wasm_bindgen_futures::JsFuture::from(promise).await.map_err(|e| WebRtcError::Peer(format!("{e:?}")))?;
        Ok(())
    }

    async fn add_ice_candidate(&self, candidate: &str) -> Result<(), WebRtcError> {
        let init = web_sys::RtcIceCandidateInit::new(candidate);
        let cand = web_sys::RtcIceCandidate::new(&init).map_err(|e| WebRtcError::Ice(format!("{e:?}")))?;
        let promise = self.inner.add_ice_candidate_with_opt_rtc_ice_candidate(Some(&cand));
        wasm_bindgen_futures::JsFuture::from(promise).await.map_err(|e| WebRtcError::Ice(format!("{e:?}")))?;
        Ok(())
    }

    fn close(&self) {
        self.inner.close();
    }
}

/// Выбор impl WebRTC (Q8): web-sys (а) vs str0m (б).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WebRtcBackend {
    /// (а) web-sys + RTCPeerConnection (браузер).
    #[default]
    WebSys,
    /// (б) str0m in wasm32.
    Str0m,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn mock_peer_creates_offer() {
        let p = MockPeer;
        let offer = p.create_offer().await.unwrap();
        assert!(offer.contains("v=0"));
    }

    #[tokio::test]
    async fn mock_peer_set_answer() {
        let p = MockPeer;
        p.set_remote_answer("v=0").await.unwrap();
        p.add_ice_candidate("candidate:...").await.unwrap();
        p.close();
    }

    #[test]
    fn default_backend_websys() {
        assert_eq!(WebRtcBackend::default(), WebRtcBackend::WebSys);
    }
}
