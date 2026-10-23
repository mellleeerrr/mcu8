//! JWT issuance and verification (ADR-0009, ADR-0021 note: production target is
//! RS256/PS256; this module ships HS256 with a config-driven secret so the crate
//! is self-contained — the RS256 adapter plugs in later behind the same API).
//!
//! Security rules (BACKEND_RULES.md §1.1, CODE_STYLE.md §3):
//! - Errors expose only the `jti`, never the full token.
//! - Claims: `sub` (user id), `jti` (unique token id), optional `room_id`, `exp`, `iat`.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use jsonwebtoken::{DecodingKey, EncodingKey, Header, Validation};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use ulid::Ulid;

/// JWT configuration (secret from `VKS_JWT_SECRET` in production).
#[derive(Debug, Clone)]
pub struct JwtConfig {
    /// HS256 signing secret. In production this comes from env `VKS_JWT_SECRET`.
    pub secret: String,
    /// Access-token lifetime in seconds (BACKEND_RULES.md: 15 min default).
    pub ttl_secs: i64,
    /// `iss` claim value.
    pub issuer: String,
}

impl Default for JwtConfig {
    fn default() -> Self {
        Self { secret: String::new(), ttl_secs: 900, issuer: "vks".to_string() }
    }
}

/// Claims carried by an access token.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Claims {
    /// Subject: user id.
    pub sub: String,
    /// Unique token id (ULID) — the only part of the token allowed in logs.
    pub jti: String,
    /// Room scope; `None` means the token is not room-bound.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub room_id: Option<String>,
    pub exp: i64,
    pub iat: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub iss: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aud: Option<String>,
    /// Monotonic generation counter to prevent same-second revocation race (M1.2).
    #[serde(default)]
    pub generation: u64,
}

/// Verification result.
#[derive(Debug, Clone)]
pub struct VerifiedJwt {
    pub user_id: String,
    pub jti: String,
    pub room_id: Option<String>,
    pub exp: i64,
    pub iat: i64,
    pub aud: Option<String>,
    pub generation: u64,
}

/// Errors for JWT operations. Messages expose only `jti` — never the token.
#[derive(Debug, Error)]
pub enum JwtError {
    #[error("jwt signing failed: {0}")]
    Signing(String),
    #[error("jwt expired (jti={0})")]
    Expired(String),
    #[error("jwt signature mismatch (jti={0})")]
    SignatureMismatch(String),
    #[error("jwt validation failed (jti={0}): {1}")]
    Validation(String, String),
    #[error("jwt missing claims (jti={0})")]
    Malformed(String),
}

/// Issues and verifies HS256 JWTs.
#[derive(Clone)]
pub struct JwtIssuer {
    config: JwtConfig,
    encoding_key: EncodingKey,
    decoding_key: DecodingKey,
}

impl std::fmt::Debug for JwtIssuer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never log the secret (BACKEND_RULES §6.3): only describe shape.
        f.debug_struct("JwtIssuer")
            .field("issuer", &self.config.issuer)
            .field("ttl_secs", &self.config.ttl_secs)
            .finish()
    }
}

impl JwtIssuer {
    /// Build an issuer from config.
    pub fn new(config: JwtConfig) -> Self {
        let encoding_key = EncodingKey::from_secret(config.secret.as_bytes());
        let decoding_key = DecodingKey::from_secret(config.secret.as_bytes());
        Self { config, encoding_key, decoding_key }
    }

    /// Issue a token for `user_id`, optionally scoped to `room_id`.
    pub fn issue(&self, user_id: &str, room_id: Option<&str>) -> Result<String, JwtError> {
        self.issue_with_gen_and_iat(user_id, room_id, 1, now_unix())
    }

    /// Issue a token for `user_id` with explicit generation and iat timestamp.
    pub fn issue_with_gen_and_iat(
        &self,
        user_id: &str,
        room_id: Option<&str>,
        generation: u64,
        iat: i64,
    ) -> Result<String, JwtError> {
        let exp = iat + self.config.ttl_secs;
        let claims = Claims {
            sub: user_id.to_string(),
            jti: Ulid::new().to_string(),
            room_id: room_id.map(str::to_string),
            exp,
            iat,
            iss: Some(self.config.issuer.clone()),
            aud: None,
            generation,
        };
        jsonwebtoken::encode(&Header::new(jsonwebtoken::Algorithm::HS256), &claims, &self.encoding_key)
            .map_err(|e| JwtError::Signing(e.to_string()))
    }

    /// Verify a token: signature + `exp`. Returns the parsed claims.
    pub fn verify(&self, token: &str) -> Result<VerifiedJwt, JwtError> {
        let mut validation = Validation::new(jsonwebtoken::Algorithm::HS256);
        // exp is checked automatically; disable aud so plain tokens pass.
        validation.validate_exp = true;
        validation.validate_aud = false;
        // Строгая проверка exp: без leeway (иначе токен, истёкший <60s, пройдёт).
        validation.leeway = 0;
        let data = jsonwebtoken::decode::<Claims>(token, &self.decoding_key, &validation)
            .map_err(|e| classify_error(e, &self.decoding_key, token))?;
        // Validate `iss`: must be present and match config.issuer (M1.2)
        match &data.claims.iss {
            Some(iss) if iss == &self.config.issuer => {}
            _ => {
                let jti = data.claims.jti.clone();
                return Err(JwtError::Validation(jti, "missing or invalid issuer".to_string()));
            }
        }
        let claims = data.claims;
        Ok(VerifiedJwt {
            user_id: claims.sub,
            jti: claims.jti,
            room_id: claims.room_id,
            exp: claims.exp,
            iat: claims.iat,
            aud: claims.aud,
            generation: claims.generation,
        })
    }
}

/// Build a `JwtError` from a decode failure. On signature failure we recover the
/// `jti` from the (unverified) payload so the error message can reference it,
/// without exposing the full token.
fn classify_error(e: jsonwebtoken::errors::Error, key: &DecodingKey, token: &str) -> JwtError {
    let jti = extract_jti(token).unwrap_or_else(|| "<unparseable>".to_string());
    let _ = key;
    match e.into_kind() {
        jsonwebtoken::errors::ErrorKind::ExpiredSignature => JwtError::Expired(jti),
        jsonwebtoken::errors::ErrorKind::InvalidSignature => JwtError::SignatureMismatch(jti),
        jsonwebtoken::errors::ErrorKind::InvalidIssuer => JwtError::Validation(jti, "issuer mismatch".to_string()),
        jsonwebtoken::errors::ErrorKind::InvalidSubject => JwtError::Validation(jti, "subject mismatch".to_string()),
        jsonwebtoken::errors::ErrorKind::MissingRequiredClaim(_) => JwtError::Malformed(jti),
        jsonwebtoken::errors::ErrorKind::InvalidToken => JwtError::Malformed(jti),
        other => JwtError::Validation(jti, format!("{other:?}")),
    }
}

/// Pull `jti` out of the base64url payload of a (possibly invalid) JWT, so errors
/// can reference the token without echoing it.
fn extract_jti(token: &str) -> Option<String> {
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() != 3 {
        return None;
    }
    let payload = base64url_decode(parts[1])?;
    let json = String::from_utf8(payload).ok()?;
    // Minimal scan for the "jti":"..." field; avoids a serde_json dependency.
    find_json_string_field(&json, "jti")
}

/// Find a string field value in a small JSON object string (minimal, no deps).
fn find_json_string_field(json: &str, field: &str) -> Option<String> {
    let needle = format!("\"{field}\":\"");
    let start = json.find(&needle)? + needle.len();
    let rest = &json[start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

/// Minimal base64url decode (no padding) for the JWT payload segment.
fn base64url_decode(input: &str) -> Option<Vec<u8>> {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = Vec::with_capacity(input.len() * 3 / 4 + 3);
    let mut buf: u32 = 0;
    let mut bits = 0;
    for b in input.bytes() {
        let idx = ALPHABET.iter().position(|&a| a == b)? as u32;
        buf = (buf << 6) | idx;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
        }
    }
    Some(out)
}

fn now_unix() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// Test helper: the same 10 s window used for expiry checks.
pub fn duration_secs(d: Duration) -> i64 {
    d.as_secs() as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn issuer(secret: &str) -> JwtIssuer {
        JwtIssuer::new(JwtConfig { secret: secret.to_string(), ttl_secs: 900, issuer: "vks".to_string() })
    }

    #[test]
    fn roundtrip_ok() {
        let a = issuer("s3cret");
        let token = a.issue("user-1", Some("room-9")).unwrap();
        let v = a.verify(&token).unwrap();
        assert_eq!(v.user_id, "user-1");
        assert_eq!(v.room_id.as_deref(), Some("room-9"));
        assert!(!v.jti.is_empty());
    }

    #[test]
    fn expired_rejected() {
        let a = JwtIssuer::new(JwtConfig {
            secret: "s".into(),
            ttl_secs: -10, // already in the past
            issuer: "vks".into(),
        });
        let token = a.issue("u", None).unwrap();
        let err = a.verify(&token).unwrap_err();
        let msg = format!("{err}");
        assert!(matches!(err, JwtError::Expired(_)));
        // message references jti but not the full token
        assert!(msg.contains("jti="), "expected jti in: {msg}");
        assert!(!msg.contains(&token), "full token leaked in error");
    }

    #[test]
    fn wrong_secret_rejected() {
        let a = issuer("secret-a");
        let b = issuer("secret-b");
        let token = a.issue("u", None).unwrap();
        let err = b.verify(&token).unwrap_err();
        assert!(matches!(err, JwtError::SignatureMismatch(_)));
        let msg = format!("{err}");
        assert!(msg.contains("jti="), "expected jti in: {msg}");
        assert!(!msg.contains(&token), "full token leaked in error");
    }

    #[test]
    fn error_exposes_jti_not_token() {
        let a = issuer("x");
        let token = a.issue("user-7", None).unwrap();
        let claims: Claims =
            jsonwebtoken::decode(&token, &DecodingKey::from_secret(b"x"), &Validation::default()).unwrap().claims;
        let wrong = issuer("y");
        let err = wrong.verify(&token).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains(&claims.jti), "jti missing from: {msg}");
        assert!(!msg.contains(&token), "full token leaked");
    }

    #[test]
    fn missing_or_wrong_issuer_rejected() {
        let a = issuer("secret");
        let claims = Claims {
            sub: "user-1".into(),
            jti: "jti-1".into(),
            room_id: None,
            exp: now_unix() + 300,
            iat: now_unix(),
            iss: None, // missing issuer!
            aud: None,
            generation: 1,
        };
        let token_missing_iss = jsonwebtoken::encode(
            &Header::new(jsonwebtoken::Algorithm::HS256),
            &claims,
            &EncodingKey::from_secret(b"secret"),
        )
        .unwrap();
        let err = a.verify(&token_missing_iss).unwrap_err();
        assert!(matches!(err, JwtError::Validation(_, ref msg) if msg.contains("issuer")));

        let b = JwtIssuer::new(JwtConfig { secret: "secret".into(), ttl_secs: 900, issuer: "other-iss".into() });
        let token_wrong_iss = b.issue("user-1", None).unwrap();
        let err2 = a.verify(&token_wrong_iss).unwrap_err();
        assert!(matches!(err2, JwtError::Validation(_, ref msg) if msg.contains("issuer")));
    }
}
