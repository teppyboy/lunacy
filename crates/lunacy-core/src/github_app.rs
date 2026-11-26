//! GitHub App authentication: RS256 JWTs and installation-token caching.

use anyhow::{Context, Result};
use base64::Engine;
use rsa::pkcs1v15::SigningKey;
use rsa::pkcs8::DecodePrivateKey;
use rsa::sha2::Sha256;
use rsa::signature::{SignatureEncoding, Signer};
use rsa::PrivateKey;
use serde_json::Value;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub const JWT_LIFETIME_SECS: u64 = 9 * 60;

/// Claims lunacy uses; GitHub only needs the three registered ones.
pub fn claims(app_id: u64, now: u64) -> serde_json::Value {
    serde_json::json!({
        "iat": now.saturating_sub(60),
        "exp": now.saturating_sub(60) + JWT_LIFETIME_SECS,
        "iss": app_id,
    })
}

fn b64url(data: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(data)
}

/// Sign `claims` as an RS256 JWT with a PKCS#8 or PKCS#1 RSA PEM key.
pub fn sign_jwt(pem: &str, claims: &serde_json::Value) -> Result<String> {
    let key = PrivateKey::from_pkcs8_pem(pem)
        .or_else(|_| {
            use rsa::pkcs1::DecodeRsaPrivateKey;
            PrivateKey::from_pkcs1_pem(pem)
        })
        .map_err(|e| anyhow::anyhow!("parsing the GitHub App private key: {e}"))?;
    let header = serde_json::json!({"alg": "RS256", "typ": "JWT"});
    let signing_input = format!(
        "{}.{}",
        b64url(header.to_string().as_bytes()),
        b64url(claims.to_string().as_bytes())
    );
    let signer = SigningKey::<Sha256>::new(key);
    let signature = signer.sign(signing_input.as_bytes());
    Ok(format!("{signing_input}.{}", b64url(&signature.to_vec())))
}

pub fn app_jwt(pem: &str, app_id: u64) -> Result<String> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    sign_jwt(pem, &claims(app_id, now))
}

/// Cached installation token with its expiry.
#[derive(Debug, Clone)]
pub struct CachedToken {
    pub token: String,
    /// Unix seconds; GitHub's `expires_at`.
    pub expires_at: u64,
}

impl CachedToken {
    /// A token stops being useful [`MARGIN`] before it truly expires.
    pub const MARGIN: u64 = 300;

    pub fn is_valid(&self, now: u64) -> bool {
        self.expires_at > now + Self::MARGIN
    }
}

/// Parse GitHub's `expires_at` (`2024-01-01T00:00:00Z`) to unix seconds.
pub fn parse_expires_at(value: &str) -> Option<u64> {
    // Minimal RFC 3339 UTC parser: YYYY-MM-DDTHH:MM:SSZ
    let b = value.as_bytes();
    if b.len() < 20 {
        return None;
    }
    let num = |s: &str| s.parse::<u64>().ok();
    let year = num(&value[0..4])?;
    let month = num(&value[5..7])?;
    let day = num(&value[8..10])?;
    let hour = num(&value[11..13])?;
    let minute = num(&value[14..16])?;
    let second = num(&value[17..19])?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    Some(
        days_from_civil(year as i64, month as i64, day as i64) as u64 * 86_400
            + hour * 3600
            + minute * 60
            + second,
    )
}

/// Howard Hinnant's days-from-civil.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// `GET /repos/{owner}/{repo}/installation` -> `id`.
pub fn parse_installation_id(body: &Value) -> Result<u64> {
    body.get("id")
        .and_then(Value::as_u64)
        .with_context(|| "installation response has no numeric `id`")
}

/// `POST /app/installations/{id}/access_tokens` -> cached token.
pub fn parse_token_response(body: &Value) -> Result<CachedToken> {
    let token = body
        .get("token")
        .and_then(Value::as_str)
        .filter(|t| !t.is_empty())
        .context("access-token response has no `token`")?;
    let expires_at = body
        .get("expires_at")
        .and_then(Value::as_str)
        .and_then(parse_expires_at)
        .context("access-token response has no parsable `expires_at`")?;
    Ok(CachedToken {
        token: token.to_string(),
        expires_at,
    })
}

/// Whether the cached token must be re-fetched at `now` (pure, so it is testable).
pub fn needs_fetch(cached: Option<&CachedToken>, now: u64) -> bool {
    match cached {
        Some(t) => !t.is_valid(now),
        None => true,
    }
}

/// One installation's cached token; refreshed when close to expiry.
#[derive(Debug, Default)]
pub struct InstallationTokenCache {
    inner: std::sync::Mutex<Option<CachedToken>>,
}

impl InstallationTokenCache {
    pub fn get(&self, now: u64) -> Option<CachedToken> {
        self.inner
            .lock()
            .ok()
            .and_then(|g| g.clone())
            .filter(|t| t.is_valid(now))
    }

    pub fn store(&self, token: CachedToken) {
        if let Ok(mut g) = self.inner.lock() {
            *g = Some(token);
        }
    }
}

pub fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_secs()
}

/// Decode a JWT's claims without verifying (tests only).
#[cfg(test)]
pub fn decode_claims_unverified(jwt: &str) -> Result<serde_json::Value> {
    use anyhow::bail;
    let mut parts = jwt.split('.');
    let (_header, payload) = match (parts.next(), parts.next()) {
        (Some(h), Some(p)) => (h, p),
        _ => bail!("not a JWT"),
    };
    let raw = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(payload)?;
    Ok(serde_json::from_slice(&raw)?)
}

/// Verify an RS256 JWT against an RSA public PEM (tests only; the daemon never verifies).
#[cfg(test)]
pub fn verify_rs256(pem: &str, jwt: &str) -> Result<bool> {
    use anyhow::bail;
    let mut parts = jwt.split('.');
    let (header, payload, sig) = match (parts.next(), parts.next(), parts.next()) {
        (Some(h), Some(p), Some(s)) => (h, p, s),
        _ => bail!("not a JWT"),
    };
    let key = rsa::pkcs8::DecodePublicKey::from_public_key_pem(pem)
        .map_err(|e| anyhow::anyhow!("parsing public key: {e}"))?;
    let sig_bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(sig)?;
    let signature = rsa::pkcs1v15::Signature::try_from(sig_bytes.as_slice())
        .map_err(|e| anyhow::anyhow!("bad signature bytes: {e}"))?;
    let verifier = rsa::pkcs1v15::VerifyingKey::<Sha256>::new(key);
    use rsa::signature::Verifier;
    Ok(verifier
        .verify(format!("{header}.{payload}").as_bytes(), &signature)
        .is_ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = include_str!("../tests/fixtures/jwt-test-key.pem");
    const PUB: &str = include_str!("../tests/fixtures/jwt-test-key.pub.pem");

    #[test]
    fn jwt_claims_and_signature() {
        let claims = claims(4242, 1_700_000_000);
        assert_eq!(claims["iss"], 4242);
        assert_eq!(claims["iat"], 1_700_000_000 - 60);
        assert_eq!(claims["exp"], 1_700_000_000 - 60 + JWT_LIFETIME_SECS);
        // GitHub rejects lifetimes over 10 minutes.
        assert!(claims["exp"].as_u64().unwrap() - claims["iat"].as_u64().unwrap() <= 600);

        let jwt = sign_jwt(KEY, &claims).unwrap();
        assert_eq!(jwt.split('.').count(), 3);
        let decoded = decode_claims_unverified(&jwt).unwrap();
        assert_eq!(decoded["iss"], 4242);
        assert!(verify_rs256(PUB, &jwt).unwrap(), "signature must verify");

        // Tampering breaks the signature.
        let mut bytes = jwt.into_bytes();
        let last = bytes.len() - 3;
        bytes[last] = if bytes[last] == b'A' { b'B' } else { b'A' };
        let tampered = String::from_utf8(bytes).unwrap();
        assert!(!verify_rs256(PUB, &tampered).unwrap_or(false));
    }

    #[test]
    fn expired_tokens_are_refreshed_before_they_die() {
        let t = CachedToken {
            token: "x".into(),
            expires_at: 10_000,
        };
        assert!(t.is_valid(9_000));
        assert!(!t.is_valid(10_000 - CachedToken::MARGIN));
        assert!(!t.is_valid(9_900));
        assert!(!needs_fetch(Some(&t), 9_000));
        assert!(needs_fetch(Some(&t), 9_800));
        assert!(needs_fetch(None, 0));
    }

    #[test]
    fn expiry_parsing() {
        // 2024-01-01T00:10:00Z
        assert_eq!(
            parse_expires_at("2024-01-01T00:10:00Z"),
            Some(1_704_067_800)
        );
        assert_eq!(parse_expires_at("garbage"), None);
        assert_eq!(parse_expires_at("2024-13-01T00:00:00Z"), None);
    }

    #[test]
    fn token_response_and_installation_parsing() {
        let body = serde_json::json!({"token": "ghs_x", "expires_at": "2024-01-01T00:10:00Z"});
        let t = parse_token_response(&body).unwrap();
        assert_eq!(t.token, "ghs_x");
        assert_eq!(t.expires_at, 1_704_067_800);
        assert!(parse_token_response(&serde_json::json!({"token": "x"})).is_err());

        assert_eq!(
            parse_installation_id(&serde_json::json!({"id": 99})).unwrap(),
            99
        );
        assert!(parse_installation_id(&serde_json::json!({})).is_err());
    }

    #[test]
    fn cache_expiry_logic() {
        let cache = InstallationTokenCache::default();
        assert!(cache.get(100).is_none());
        cache.store(CachedToken {
            token: "a".into(),
            expires_at: 1000,
        });
        assert_eq!(cache.get(500).unwrap().token, "a");
        assert!(cache.get(800).is_none());
    }
}
