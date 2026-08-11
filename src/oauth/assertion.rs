//! JWT assertions per RFC 7523.
//!
//! One assertion is minted per token request rather than cached: it is a
//! single-use credential with a lifetime measured in seconds, and signing one is
//! orders of magnitude cheaper than the round trip it accompanies.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::Context as _;
use jsonwebtoken::Header;
use serde::Serialize;
use uuid::Uuid;

use super::key::SigningKey;

/// The static inputs of every assertion signed for one grant.
pub struct AssertionConfig {
    /// `iss` — who issued the assertion. For client authentication (RFC 7523
    /// §2.2) this is the client id.
    pub issuer: String,
    /// `sub` — the principal the assertion speaks for. Equal to `issuer` for
    /// client authentication; a distinct value is what makes an authorization
    /// grant (§2.1) act on someone else's behalf.
    pub subject: String,
    /// `aud` — the authorization server the assertion is addressed to. RFC 7523
    /// §3 requires the AS to reject an assertion that is not addressed to it,
    /// which is what stops one server from replaying it against another.
    pub audience: String,
    /// How long the assertion stays valid, from `iat` to `exp`.
    pub lifetime: Duration,
}

/// The claims of RFC 7523 §3. Serialised in this order for readability when a
/// provider echoes the assertion back in an error.
#[derive(Serialize)]
struct Claims<'a> {
    iss: &'a str,
    sub: &'a str,
    aud: &'a str,
    iat: u64,
    exp: u64,
    /// §3 point 7 — a unique identifier, so the authorization server can reject
    /// a replayed assertion. Fresh per assertion, never reused.
    jti: String,
}

/// Mint and sign one assertion.
pub fn sign(config: &AssertionConfig, key: &SigningKey) -> anyhow::Result<String> {
    let issued_at = unix_now()?;
    let claims = Claims {
        iss: &config.issuer,
        sub: &config.subject,
        aud: &config.audience,
        iat: issued_at,
        // Saturating rather than wrapping: an absurd lifetime should produce a
        // far-future expiry the AS will reject, not an expiry in the past.
        exp: issued_at.saturating_add(config.lifetime.as_secs()),
        jti: Uuid::new_v4().to_string(),
    };

    let mut header = Header::new(key.algorithm);
    header.kid = key.key_id.clone();
    jsonwebtoken::encode(&header, &claims, &key.key).context("signing the OAuth client assertion")
}

/// Seconds since the Unix epoch.
fn unix_now() -> anyhow::Result<u64> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("the system clock is set before the Unix epoch")?
        .as_secs())
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode, decode_header};
    use serde_json::Value;

    use super::*;
    use crate::cli::SigningAlg;

    /// The modulus of the throwaway RSA fixture in `tests/fixtures`, so the
    /// assertion can be verified here with the matching public key. Same value
    /// as `auth::tests::TEST_N` — if it ever drifts, these tests stop verifying.
    const RSA_N: &str = "pIrAmCcbgl0Z6Fmomx9TVpVhMiOjJOrtjzKHoKnV5pYyFz86Zpor4tHmK8inQB6ES7j2V-0cgnT-62g_wCCwJHS-jJY0GawNgkxPq_5zFSFBuhJjyGpQzofexEPP7Qof6ZQKRViNw5A64C-dkcgoixhOBS1TWk6mkDOgoYOv9q2IUM5saRYZIwQw7OU4hsKetZcq8gbmVSjbzPylFryaIu5Udlo4JxFt-7t0RG_N858nu6eBYR68KMlOZIqN4YsaaQBm6teCdOUUXxAww8Yuij0gbz_YXMSnu5A5Ooff8w83kQLJqPLJyyEb357CvCqZsDZmlp3LFVRRmNuDPUtTKQ";

    /// Public half of the throwaway EC fixture, to verify an `es256` assertion.
    const EC_PUBLIC_PEM: &str = "\
-----BEGIN PUBLIC KEY-----
MFkwEwYHKoZIzj0CAQYIKoZIzj0DAQcDQgAEXGvAHlS6XVytLWduCmNUVGg0Gays
F/crinjkiv2S3P1TaHq9KSGfuuuE0GC9lLm0odTYSg+bsG3StBziDwCr+w==
-----END PUBLIC KEY-----
";

    fn config() -> AssertionConfig {
        AssertionConfig {
            issuer: "client-abc".into(),
            subject: "client-abc".into(),
            audience: "https://idp.example.com/token".into(),
            lifetime: Duration::from_secs(60),
        }
    }

    fn rsa_key(key_id: Option<&str>) -> SigningKey {
        super::super::key::load(
            Path::new("tests/fixtures/test_rsa_key.pem"),
            SigningAlg::Rs256,
            key_id.map(str::to_string),
        )
        .expect("the RSA fixture loads")
    }

    /// Verify against the fixture's public key and return the claims.
    fn verify_rsa(token: &str, audience: &str) -> Value {
        let key = DecodingKey::from_rsa_components(RSA_N, "AQAB").expect("public key builds");
        let mut validation = Validation::new(Algorithm::RS256);
        validation.set_audience(&[audience]);
        validation.set_required_spec_claims(&["exp", "aud"]);
        decode::<Value>(token, &key, &validation)
            .expect("the assertion verifies")
            .claims
    }

    #[test]
    fn signs_an_assertion_carrying_the_rfc_7523_claims() {
        let config = config();
        let token = sign(&config, &rsa_key(None)).expect("signing succeeds");
        let claims = verify_rsa(&token, &config.audience);

        // For client authentication, `iss` and `sub` are both the client id.
        assert_eq!(claims["iss"], "client-abc");
        assert_eq!(claims["sub"], "client-abc");
        assert_eq!(claims["aud"], "https://idp.example.com/token");
        assert!(claims["jti"].as_str().is_some_and(|jti| !jti.is_empty()));

        let iat = claims["iat"].as_u64().expect("iat is a number");
        let exp = claims["exp"].as_u64().expect("exp is a number");
        assert_eq!(
            exp - iat,
            60,
            "exp must be iat plus the configured lifetime"
        );
        assert!(
            iat <= unix_now().expect("clock") && exp > iat,
            "iat {iat} / exp {exp} must bracket now"
        );
    }

    #[test]
    fn the_header_carries_the_algorithm_and_the_optional_kid() {
        let with_kid = sign(&config(), &rsa_key(Some("kid-7"))).expect("signing succeeds");
        let header = decode_header(&with_kid).expect("header decodes");
        assert_eq!(header.alg, Algorithm::RS256);
        assert_eq!(header.kid.as_deref(), Some("kid-7"));

        // No `kid` configured means no `kid` header — some providers reject an
        // assertion naming a key they do not know.
        let without = sign(&config(), &rsa_key(None)).expect("signing succeeds");
        assert!(
            decode_header(&without)
                .expect("header decodes")
                .kid
                .is_none()
        );
    }

    #[test]
    fn every_assertion_gets_a_fresh_jti() {
        let key = rsa_key(None);
        let config = config();
        let first = verify_rsa(&sign(&config, &key).expect("first"), &config.audience);
        let second = verify_rsa(&sign(&config, &key).expect("second"), &config.audience);
        // A reused `jti` is exactly what an AS's replay cache rejects.
        assert_ne!(first["jti"], second["jti"]);
    }

    #[test]
    fn a_wrong_audience_fails_verification() {
        let config = config();
        let token = sign(&config, &rsa_key(None)).expect("signing succeeds");
        let key = DecodingKey::from_rsa_components(RSA_N, "AQAB").expect("public key builds");
        let mut validation = Validation::new(Algorithm::RS256);
        validation.set_audience(&["https://other.example.com/token"]);
        assert!(
            decode::<Value>(&token, &key, &validation).is_err(),
            "an assertion addressed elsewhere must not verify"
        );
    }

    #[test]
    fn signs_with_an_ec_key_too() {
        let key = super::super::key::load(
            Path::new("tests/fixtures/test_ec_key.pem"),
            SigningAlg::Es256,
            None,
        )
        .expect("the EC fixture loads");
        let config = config();
        let token = sign(&config, &key).expect("signing with an EC key succeeds");

        let decoding =
            DecodingKey::from_ec_pem(EC_PUBLIC_PEM.as_bytes()).expect("EC public key builds");
        let mut validation = Validation::new(Algorithm::ES256);
        validation.set_audience(&[config.audience.as_str()]);
        let claims = decode::<Value>(&token, &decoding, &validation)
            .expect("the ES256 assertion verifies")
            .claims;
        assert_eq!(claims["iss"], "client-abc");
    }

    #[test]
    fn a_distinct_subject_is_carried_through() {
        // Client authentication keeps iss == sub, but the claim is the caller's
        // to choose — this is what an authorization grant will vary.
        let config = AssertionConfig {
            subject: "user-42".into(),
            ..config()
        };
        let token = sign(&config, &rsa_key(None)).expect("signing succeeds");
        let claims = verify_rsa(&token, &config.audience);
        assert_eq!(claims["iss"], "client-abc");
        assert_eq!(claims["sub"], "user-42");
    }
}
