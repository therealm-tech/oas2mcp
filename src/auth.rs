//! Per-request, role-based tool authorization.
//!
//! When `--oauth-role-mapper` is configured, the incoming MCP request's
//! `Authorization: Bearer` JWT is verified against a JWKS and decoded. The
//! caller's roles are read from a configurable claim, and each `role` is mapped
//! to a regex over tool names: a tool is visible and callable only if one of
//! the caller's roles maps to a regex matching the tool's name. A caller with
//! no valid token — or whose roles match nothing — gets an empty tool set.

use std::collections::HashSet;
use std::sync::Arc;

use anyhow::{Context as _, bail};
use jsonwebtoken::jwk::{AlgorithmParameters, Jwk, JwkSet};
use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode, decode_header};
use regex::Regex;
use serde_json::{Map, Value};

use crate::cli::Cli;

/// One `role:tool_name_regex` mapping: a caller holding `role` may use any tool
/// whose name matches `pattern`.
struct RoleRule {
    role: String,
    pattern: Regex,
}

/// The claims read from a verified token: the caller's roles and the
/// `--trace-claim` claims selected for tracing.
pub struct VerifiedClaims {
    pub roles: HashSet<String>,
    /// The `--trace-claim` claims that were present in the token, keeping their
    /// JSON shape. Empty when none are configured or none were present. Surfaced
    /// on the per-call tracing log, never in metric labels.
    pub traced: Map<String, Value>,
}

/// Verifies incoming JWTs against a JWKS and decides which tools a caller may
/// use based on the roles in the token. Built once at startup and shared (via
/// an [`Arc`]) across every per-session server clone.
pub struct Authorizer {
    jwks: JwkSet,
    role_claim: String,
    rules: Vec<RoleRule>,
    /// Names of the claims to copy into the per-call tracing log, from
    /// `--trace-claim`. Empty disables claim tracing.
    trace_claims: Vec<String>,
}

impl Authorizer {
    /// Build the authorizer from the CLI, or `None` when `--oauth-role-mapper`
    /// is not set (no authorization, every tool exposed). Fetches the JWKS from
    /// the configured URL or reads it from disk — this is why it is async.
    pub async fn from_cli(cli: &Cli) -> anyhow::Result<Option<Arc<Self>>> {
        if cli.oauth_role_mapper.is_empty() {
            // No mapper means no JWKS is needed even if one was passed; surface
            // that as a misconfiguration rather than silently ignoring it.
            if cli.oauth_jwks_url.is_some() || cli.oauth_jwks_file.is_some() {
                tracing::warn!(
                    "--oauth-jwks-url/--oauth-jwks-file is set but --oauth-role-mapper is not; \
                     no JWT authorization is enforced"
                );
            }
            return Ok(None);
        }

        let rules = parse_rules(&cli.oauth_role_mapper)?;
        let jwks = load_jwks(cli).await?;
        Ok(Some(Arc::new(Self {
            jwks,
            role_claim: cli.oauth_role_claim.clone(),
            rules,
            trace_claims: cli.trace_claims.clone(),
        })))
    }

    /// Verify `token` against the JWKS and return the claims it carries (roles
    /// and `sub`). Fails when the token is malformed, signed by an unknown key,
    /// expired, or otherwise fails verification.
    pub fn verify(&self, token: &str) -> anyhow::Result<VerifiedClaims> {
        let header = decode_header(token).context("decoding the JWT header")?;
        let kid = header
            .kid
            .context("the JWT has no `kid` header, cannot select a verification key")?;
        let jwk = self
            .jwks
            .find(&kid)
            .with_context(|| format!("no JWK in the set matches the token's kid `{kid}`"))?;

        let key = DecodingKey::from_jwk(jwk).context("building a decoding key from the JWK")?;
        // Constrain the accepted algorithms to those of the key's family so a
        // forged token cannot downgrade to e.g. HS256 against a public key.
        let mut validation = Validation::new(algorithm_for(jwk)?);
        validation.algorithms = algorithms_for(jwk)?;
        // Audience/issuer are out of scope here; we only authenticate the
        // signature and expiry and then map the roles claim.
        validation.validate_aud = false;

        let data = decode::<Value>(token, &key, &validation).context("verifying the JWT")?;
        Ok(VerifiedClaims {
            roles: extract_roles(&data.claims, &self.role_claim),
            traced: extract_traced_claims(&data.claims, &self.trace_claims),
        })
    }

    /// Whether a caller holding `roles` is allowed to use the tool named `tool`.
    pub fn allows(&self, roles: &HashSet<String>, tool: &str) -> bool {
        self.rules
            .iter()
            .any(|rule| roles.contains(&rule.role) && rule.pattern.is_match(tool))
    }
}

/// Parse `role:tool_name_regex` entries, validating each regex at startup.
fn parse_rules(raw: &[String]) -> anyhow::Result<Vec<RoleRule>> {
    raw.iter()
        .map(|entry| {
            let (role, pattern) = entry.split_once(':').with_context(|| {
                format!("role mapping `{entry}` is not in `role:tool_name_regex` form")
            })?;
            let role = role.trim();
            if role.is_empty() {
                bail!("role mapping `{entry}` has an empty role");
            }
            let pattern = Regex::new(pattern)
                .with_context(|| format!("invalid tool-name regex in role mapping `{entry}`"))?;
            Ok(RoleRule {
                role: role.to_string(),
                pattern,
            })
        })
        .collect()
}

/// Load the JWKS from the configured URL (fetched at startup) or file. Exactly
/// one source must be set when a role mapper is configured.
async fn load_jwks(cli: &Cli) -> anyhow::Result<JwkSet> {
    let bytes = match (&cli.oauth_jwks_url, &cli.oauth_jwks_file) {
        (Some(url), _) => {
            tracing::debug!(%url, "fetching JWKS for JWT verification");
            let client = crate::http::client(cli).context("building the JWKS HTTP client")?;
            client
                .get(url.clone())
                .send()
                .await
                .with_context(|| format!("fetching JWKS from {url}"))?
                .error_for_status()
                .with_context(|| format!("JWKS request to {url} failed"))?
                .bytes()
                .await
                .with_context(|| format!("reading JWKS response body from {url}"))?
                .to_vec()
        }
        (None, Some(path)) => {
            tracing::debug!(path = %path.display(), "reading JWKS from file");
            tokio::fs::read(path)
                .await
                .with_context(|| format!("reading JWKS file {}", path.display()))?
        }
        (None, None) => bail!(
            "--oauth-role-mapper is set but no JWKS source was given; \
             pass --oauth-jwks-url or --oauth-jwks-file"
        ),
    };

    serde_json::from_slice(&bytes).context("parsing the JWKS document")
}

/// The primary verification algorithm for a JWK, picked from its key type.
fn algorithm_for(jwk: &Jwk) -> anyhow::Result<Algorithm> {
    Ok(match &jwk.algorithm {
        AlgorithmParameters::RSA(_) => Algorithm::RS256,
        AlgorithmParameters::EllipticCurve(_) => Algorithm::ES256,
        AlgorithmParameters::OctetKeyPair(_) => Algorithm::EdDSA,
        AlgorithmParameters::OctetKey(_) => {
            bail!("symmetric (oct) JWKs are not supported for token verification")
        }
    })
}

/// All algorithms a JWK's key family may legitimately use. Restricting the
/// validation to this set blocks algorithm-substitution attacks.
fn algorithms_for(jwk: &Jwk) -> anyhow::Result<Vec<Algorithm>> {
    Ok(match &jwk.algorithm {
        AlgorithmParameters::RSA(_) => vec![
            Algorithm::RS256,
            Algorithm::RS384,
            Algorithm::RS512,
            Algorithm::PS256,
            Algorithm::PS384,
            Algorithm::PS512,
        ],
        AlgorithmParameters::EllipticCurve(_) => vec![Algorithm::ES256, Algorithm::ES384],
        AlgorithmParameters::OctetKeyPair(_) => vec![Algorithm::EdDSA],
        AlgorithmParameters::OctetKey(_) => {
            bail!("symmetric (oct) JWKs are not supported for token verification")
        }
    })
}

/// Read the roles out of the configured claim. The claim may be a JSON array of
/// strings or a single whitespace-separated string; anything else yields no
/// roles.
fn extract_roles(claims: &Value, claim: &str) -> HashSet<String> {
    match claims.get(claim) {
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect(),
        Some(Value::String(s)) => s.split_whitespace().map(str::to_string).collect(),
        _ => HashSet::new(),
    }
}

/// Pick the `--trace-claim` claims out of the token, preserving each value's
/// JSON shape (string, number, array, …). Claims absent from the token are
/// skipped — only what was actually present is traced.
fn extract_traced_claims(claims: &Value, names: &[String]) -> Map<String, Value> {
    names
        .iter()
        .filter_map(|name| claims.get(name).map(|value| (name.clone(), value.clone())))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn roles(values: &[&str]) -> HashSet<String> {
        values.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parse_rules_splits_on_the_first_colon() {
        // A regex may itself contain a colon; only the first one separates the
        // role from the pattern.
        let rules = parse_rules(&["admin:^get:.*".into()]).expect("valid mapping");
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].role, "admin");
        assert!(rules[0].pattern.is_match("get:thing"));
    }

    #[test]
    fn parse_rules_rejects_missing_colon_and_bad_regex() {
        assert!(parse_rules(&["adminonly".into()]).is_err());
        assert!(parse_rules(&["admin:(".into()]).is_err());
        assert!(parse_rules(&[":^get".into()]).is_err());
    }

    #[test]
    fn allows_matches_role_and_pattern() {
        let authz = Authorizer {
            jwks: JwkSet { keys: vec![] },
            role_claim: "roles".into(),
            rules: parse_rules(&["admin:.*".into(), "reader:^get".into()]).expect("valid mappings"),
            trace_claims: vec![],
        };

        // admin can use anything.
        assert!(authz.allows(&roles(&["admin"]), "deletePet"));
        // reader only the read tools.
        assert!(authz.allows(&roles(&["reader"]), "getPet"));
        assert!(!authz.allows(&roles(&["reader"]), "deletePet"));
        // unknown role: nothing.
        assert!(!authz.allows(&roles(&["guest"]), "getPet"));
        // no roles at all: nothing.
        assert!(!authz.allows(&roles(&[]), "getPet"));
    }

    #[test]
    fn extract_roles_handles_array_string_and_absent() {
        assert_eq!(
            extract_roles(&json!({ "roles": ["a", "b"] }), "roles"),
            roles(&["a", "b"])
        );
        assert_eq!(
            extract_roles(&json!({ "roles": "a b" }), "roles"),
            roles(&["a", "b"])
        );
        assert!(extract_roles(&json!({ "roles": 42 }), "roles").is_empty());
        assert!(extract_roles(&json!({}), "roles").is_empty());
    }

    #[test]
    fn extract_traced_claims_keeps_present_claims_in_their_json_shape() {
        let claims = json!({
            "sub": "user-123",
            "email": "a@b.com",
            "tenant_id": 42,
            "groups": ["x", "y"],
        });
        let traced = extract_traced_claims(
            &claims,
            &[
                "sub".into(),
                "tenant_id".into(),
                "groups".into(),
                "missing".into(),
            ],
        );
        assert_eq!(traced.get("sub"), Some(&json!("user-123")));
        assert_eq!(traced.get("tenant_id"), Some(&json!(42)));
        assert_eq!(traced.get("groups"), Some(&json!(["x", "y"])));
        // Absent claims and claims not requested are left out.
        assert!(!traced.contains_key("missing"));
        assert!(!traced.contains_key("email"));
        // No names configured → nothing traced.
        assert!(extract_traced_claims(&claims, &[]).is_empty());
    }

    // A throwaway 2048-bit RSA keypair generated solely for these tests, with
    // the matching JWK modulus. NOT a real credential — never used outside the
    // test. The PEM lives in a fixture file (and is excluded from the
    // detect-private-key hook) rather than inline so it cannot be mistaken for a
    // leaked production key.
    const TEST_KID: &str = "test-key";
    const TEST_N: &str = "pIrAmCcbgl0Z6Fmomx9TVpVhMiOjJOrtjzKHoKnV5pYyFz86Zpor4tHmK8inQB6ES7j2V-0cgnT-62g_wCCwJHS-jJY0GawNgkxPq_5zFSFBuhJjyGpQzofexEPP7Qof6ZQKRViNw5A64C-dkcgoixhOBS1TWk6mkDOgoYOv9q2IUM5saRYZIwQw7OU4hsKetZcq8gbmVSjbzPylFryaIu5Udlo4JxFt-7t0RG_N858nu6eBYR68KMlOZIqN4YsaaQBm6teCdOUUXxAww8Yuij0gbz_YXMSnu5A5Ooff8w83kQLJqPLJyyEb357CvCqZsDZmlp3LFVRRmNuDPUtTKQ";
    const TEST_PRIV_PEM: &str = include_str!("../tests/fixtures/test_rsa_key.pem");

    fn test_authorizer() -> Authorizer {
        let jwks: JwkSet = serde_json::from_value(json!({
            "keys": [{
                "kty": "RSA",
                "use": "sig",
                "alg": "RS256",
                "kid": TEST_KID,
                "n": TEST_N,
                "e": "AQAB",
            }]
        }))
        .expect("test JWKS parses");
        Authorizer {
            jwks,
            role_claim: "roles".into(),
            rules: parse_rules(&["admin:.*".into()]).expect("valid mapping"),
            trace_claims: vec!["sub".into(), "email".into()],
        }
    }

    /// Sign a token with the test key, `kid` header, a fixed `sub`, and the
    /// given expiry.
    fn sign(roles_claim: Value, exp_unix: u64, kid: Option<&str>) -> String {
        use jsonwebtoken::{EncodingKey, Header};
        let mut header = Header::new(Algorithm::RS256);
        header.kid = kid.map(str::to_string);
        let claims = json!({ "roles": roles_claim, "sub": "user-123", "exp": exp_unix });
        let key = EncodingKey::from_rsa_pem(TEST_PRIV_PEM.as_bytes()).expect("test key parses");
        jsonwebtoken::encode(&header, &claims, &key).expect("signing succeeds")
    }

    fn in_one_hour() -> u64 {
        use std::time::{SystemTime, UNIX_EPOCH};
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
            + 3600
    }

    #[test]
    fn verify_reads_roles_and_traced_claims_from_a_valid_jwt() {
        let authz = test_authorizer();
        let token = sign(json!(["admin", "reader"]), in_one_hour(), Some(TEST_KID));
        let claims = authz.verify(&token).expect("valid token verifies");
        assert_eq!(claims.roles, roles(&["admin", "reader"]));
        assert!(authz.allows(&claims.roles, "deletePet"));
        // `sub` is configured for tracing and present; `email` is configured but
        // absent from the token, so it is not traced.
        assert_eq!(claims.traced.get("sub"), Some(&json!("user-123")));
        assert!(!claims.traced.contains_key("email"));
    }

    #[test]
    fn verify_rejects_expired_and_tampered_tokens() {
        let authz = test_authorizer();

        // Expired: exp one hour in the past.
        let expired = sign(json!(["admin"]), in_one_hour() - 7200, Some(TEST_KID));
        assert!(authz.verify(&expired).is_err());

        // Tampered signature: flip the last character of a valid token.
        let mut token = sign(json!(["admin"]), in_one_hour(), Some(TEST_KID)).into_bytes();
        let last = token.last_mut().unwrap();
        *last = if *last == b'A' { b'B' } else { b'A' };
        let tampered = String::from_utf8(token).unwrap();
        assert!(authz.verify(&tampered).is_err());

        // Unknown kid: no matching JWK.
        let unknown = sign(json!(["admin"]), in_one_hour(), Some("other-key"));
        assert!(authz.verify(&unknown).is_err());
    }
}
