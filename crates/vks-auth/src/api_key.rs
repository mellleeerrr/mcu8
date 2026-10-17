//! API keys (ADR-0009, BACKEND_RULES.md §1.2).
//!
//! Format: `vks_` + 32 hex chars (128 bits of randomness). Only the SHA-256 hash
//! is stored; the plaintext key is shown once at creation. The public `prefix`
//! (first 8 chars, i.e. `vks_` + 4 hex) is the only part allowed in logs.

use rand::RngCore;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

/// Length of the hex part of an API key (32 hex chars = 16 bytes).
pub const KEY_HEX_LEN: usize = 32;

/// Prefix of every API key.
pub const KEY_PREFIX: &str = "vks_";

/// A stored API key: SHA-256 hash plus display metadata.
#[derive(Debug, Clone)]
pub struct ApiKey {
    /// Opaque key identifier (e.g. ULID string) — the value `verify` returns.
    pub key_id: String,
    /// SHA-256 of the full key string, hex-encoded (64 chars).
    pub hash_hex: String,
    /// First 8 chars of the plaintext key (`vks_` + 4 hex) for display/logging.
    pub prefix: String,
}

impl ApiKey {
    /// Constant-time compare of a presented key against the stored hash.
    fn matches(&self, presented: &str) -> bool {
        let presented_hash = sha256_hex(presented.as_bytes());
        let a = hex_to_bytes(&self.hash_hex).unwrap_or_default();
        let b = hex_to_bytes(&presented_hash).unwrap_or_default();
        a.len() == b.len() && a.ct_eq(&b).into()
    }

    /// True when `presented` is this key.
    pub fn verify(&self, presented: &str) -> bool {
        self.matches(presented)
    }
}

/// Generate a new API key. Returns the one-time plaintext and its stored form.
pub fn generate(key_id: &str) -> (String, ApiKey) {
    let mut bytes = [0u8; KEY_HEX_LEN / 2];
    rand::thread_rng().fill_bytes(&mut bytes);
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    let key = format!("{KEY_PREFIX}{hex}");
    let hash_hex = sha256_hex(key.as_bytes());
    let prefix = key.chars().take(8).collect();
    (key, ApiKey { key_id: key_id.to_string(), hash_hex, prefix })
}

/// Constant-time verify a presented key against a stored record.
/// Returns `Some(key_id)` on match, `None` otherwise.
pub fn verify(key: &str, stored: &ApiKey) -> Option<String> {
    if stored.verify(key) {
        Some(stored.key_id.clone())
    } else {
        None
    }
}

/// Display prefix of a plaintext key (first 8 chars).
pub fn prefix(key: &str) -> String {
    key.chars().take(8).collect()
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

fn hex_to_bytes(hex: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(hex.len() / 2);
    let bytes = hex.as_bytes();
    let mut i = 0;
    while i + 1 < bytes.len() {
        let hi = hex_val(bytes[i])?;
        let lo = hex_val(bytes[i + 1])?;
        out.push((hi << 4) | lo);
        i += 2;
    }
    Some(out)
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_is_vks_plus_32_hex() {
        let (key, _stored) = generate("id-1");
        assert!(key.starts_with("vks_"), "got {key}");
        let hex = &key["vks_".len()..];
        assert_eq!(hex.len(), KEY_HEX_LEN, "hex len was {}", hex.len());
        assert!(hex.chars().all(|c| c.is_ascii_hexdigit()), "non-hex in {key}");
    }

    #[test]
    fn prefix_len_is_eight() {
        let (key, stored) = generate("id-2");
        assert_eq!(prefix(&key).len(), 8);
        assert_eq!(stored.prefix.len(), 8);
        assert_eq!(stored.prefix, prefix(&key));
    }

    #[test]
    fn verify_matches_stored_hash() {
        let (key, stored) = generate("id-3");
        assert_eq!(verify(&key, &stored).as_deref(), Some("id-3"));
    }

    #[test]
    fn wrong_key_rejected() {
        let (key, stored) = generate("id-4");
        let mut wrong = key.clone();
        // flip last hex char
        let last = wrong.len() - 1;
        wrong.replace_range(last..last + 1, if wrong.as_bytes()[last] == b'0' { "1" } else { "0" });
        assert_ne!(wrong, key);
        assert_eq!(verify(&wrong, &stored), None);
    }

    #[test]
    fn stored_is_sha256_of_key() {
        let (key, stored) = generate("id-5");
        let expected = sha256_hex(key.as_bytes());
        assert_eq!(stored.hash_hex, expected);
        // plain key is not stored
        assert_ne!(stored.hash_hex, key);
    }
}
