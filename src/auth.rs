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
use std::time::Duration;

use anyhow::{Context as _, bail};
use jsonwebtoken::jwk::{AlgorithmParameters, Jwk, JwkSet};
use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode, decode_header};
use regex::Regex;
use serde_json::{Map, Value};

use crate::cli::Cli;

/// Clock skew tolerated on `exp`/`nbf` when none is configured. Matches
/// `jsonwebtoken`'s own default, so the flag changes nothing until it is set.
const DEFAULT_CLOCK_SKEW: Duration = Duration::from_secs(60);

/// One `role:tool_name_regex` mapping: a caller holding `role` may use any tool
/// whose name matches `pattern`.
struct RoleRule {
    role: String,
    pattern: Regex,
}

/// The claims read from a verified token: the caller's roles, the identity used
/// for delegated upstream tokens, and the `--trace-claim` claims selected for
/// tracing.
pub struct VerifiedClaims {
    pub roles: HashSet<String>,
    /// The `--trace-claim` claims that were present in the token, keeping their
    /// JSON shape. Empty when none are configured or none were present. Surfaced
    /// on the per-call tracing log, never in metric labels.
    pub traced: Map<String, Value>,
    /// Value of the delegation subject claim (`--upstream-oauth-subject-claim`,
    /// `sub` by default). `None` when the claim is absent or is not a string —
    /// which denies delegation rather than falling back to another identity.
    pub subject: Option<String>,
    /// The token's `iss`. Part of the delegated-token cache key: `sub` is only
    /// unique *within* an issuer, so two providers both minting `sub: alice`
    /// must not share a cache entry.
    pub issuer: Option<String>,
    /// The token's `exp`, in seconds since the Unix epoch. A delegated upstream
    /// token is never cached past the caller session it was minted for.
    pub expiry: Option<u64>,
}

/// Verifies incoming JWTs against a JWKS and decides which tools a caller may
/// use based on the roles in the token. Built once at startup and shared (via
/// an [`Arc`]) across every per-session server clone.
pub struct Authorizer {
    jwks: JwkSet,
    role_claim: String,
    /// Audiences the token's `aud` may match. Empty disables the check, which is
    /// the pre-existing behaviour and accepts a token addressed to any service
    /// the JWKS happens to cover.
    expected_audiences: Vec<String>,
    /// Issuers the token's `iss` may match. Empty disables the check.
    expected_issuers: Vec<String>,
    /// Skew tolerated on `exp`/`nbf`.
    clock_skew: Duration,
    /// Claim carrying the identity to delegate as. Lives here because this is
    /// where the token is decoded, even though the flag that sets it belongs to
    /// the upstream OAuth group.
    subject_claim: String,
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
        if cli.oauth_expected_audiences.is_empty() {
            // Not an error: rejecting these tokens outright would break every
            // deployment that predates the flag. But it *is* worth saying, since
            // it means a token minted for another service passes here.
            tracing::warn!(
                "--oauth-expected-audience is not set; any JWT this JWKS can verify is accepted, \
                 including one issued for a different service. Set it to scope tokens to this \
                 server."
            );
        }
        Ok(Some(Arc::new(Self {
            jwks,
            role_claim: cli.oauth_role_claim.clone(),
            expected_audiences: cli.oauth_expected_audiences.clone(),
            expected_issuers: cli.oauth_expected_issuers.clone(),
            clock_skew: cli.oauth_clock_skew.unwrap_or(DEFAULT_CLOCK_SKEW),
            subject_claim: cli.upstream_oauth_subject_claim.clone(),
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
        validation.leeway = self.clock_skew.as_secs();

        // `aud` and `iss` are only checked when an expectation is configured —
        // and whenever one is, the claim also becomes **mandatory**. Neither
        // `set_audience` nor `set_issuer` does that on its own, and without it a
        // token that simply omits the claim satisfies the constraint by default:
        // the check would be bypassable by leaving the claim out.
        let mut required = vec!["exp"];
        validation.validate_aud = !self.expected_audiences.is_empty();
        if !self.expected_audiences.is_empty() {
            validation.set_audience(&self.expected_audiences);
            required.push("aud");
        }
        if !self.expected_issuers.is_empty() {
            validation.set_issuer(&self.expected_issuers);
            required.push("iss");
        }
        validation.set_required_spec_claims(&required);

        let data = decode::<Value>(token, &key, &validation).context("verifying the JWT")?;
        Ok(VerifiedClaims {
            roles: extract_roles(&data.claims, &self.role_claim),
            traced: extract_traced_claims(&data.claims, &self.trace_claims),
            subject: extract_string(&data.claims, &self.subject_claim),
            issuer: extract_string(&data.claims, "iss"),
            expiry: data.claims.get("exp").and_then(Value::as_u64),
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
        // `AlgorithmParameters` is `#[non_exhaustive]`: reject key types this
        // build does not know rather than guessing an algorithm for them.
        other => bail!("unsupported JWK key type {other:?} for token verification"),
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
        // See `algorithm_for`: unknown key types get no algorithm allow-list.
        other => bail!("unsupported JWK key type {other:?} for token verification"),
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

/// Read a claim that must be a string. A claim of any other shape yields `None`
/// rather than a stringified value: an identity derived from coercing a number
/// or an array is not an identity anyone registered with a provider.
fn extract_string(claims: &Value, claim: &str) -> Option<String> {
    match claims.get(claim) {
        Some(Value::String(value)) if !value.is_empty() => Some(value.clone()),
        _ => None,
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
            subject_claim: "sub".into(),
            expected_audiences: Vec::new(),
            expected_issuers: Vec::new(),
            clock_skew: DEFAULT_CLOCK_SKEW,
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
            subject_claim: "sub".into(),
            expected_audiences: Vec::new(),
            expected_issuers: Vec::new(),
            clock_skew: DEFAULT_CLOCK_SKEW,
            rules: parse_rules(&["admin:.*".into()]).expect("valid mapping"),
            trace_claims: vec!["sub".into(), "email".into()],
        }
    }

    /// Sign a token with the test key, `kid` header, a fixed `sub`, and the
    /// given expiry.
    fn sign(roles_claim: Value, exp_unix: u64, kid: Option<&str>) -> String {
        sign_claims(
            json!({ "roles": roles_claim, "sub": "user-123", "exp": exp_unix }),
            kid,
        )
    }

    /// Sign an arbitrary claim set with the test key.
    fn sign_claims(claims: Value, kid: Option<&str>) -> String {
        use jsonwebtoken::{EncodingKey, Header};
        let mut header = Header::new(Algorithm::RS256);
        header.kid = kid.map(str::to_string);
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

    #[test]
    fn verify_reads_the_delegation_identity() {
        let authz = test_authorizer();
        let exp = in_one_hour();
        let token = sign_claims(
            json!({
                "roles": ["admin"],
                "sub": "user-123",
                "iss": "https://idp.example.com/",
                "exp": exp,
            }),
            Some(TEST_KID),
        );
        let claims = authz.verify(&token).expect("valid token verifies");
        assert_eq!(claims.subject.as_deref(), Some("user-123"));
        // The issuer scopes the subject: it is half of the delegated cache key.
        assert_eq!(claims.issuer.as_deref(), Some("https://idp.example.com/"));
        assert_eq!(claims.expiry, Some(exp));
    }

    #[test]
    fn the_delegation_subject_can_come_from_another_claim() {
        // Many providers mint an opaque `sub` the upstream does not know, so the
        // claim is configurable.
        let mut authz = test_authorizer();
        authz.subject_claim = "email".into();
        let token = sign_claims(
            json!({ "roles": ["admin"], "sub": "opaque-guid", "email": "a@b.com", "exp": in_one_hour() }),
            Some(TEST_KID),
        );
        let claims = authz.verify(&token).expect("valid token verifies");
        assert_eq!(claims.subject.as_deref(), Some("a@b.com"));
    }

    #[test]
    fn a_missing_or_non_string_subject_claim_denies_delegation() {
        let authz = test_authorizer();

        // Absent: no identity to act as.
        let token = sign_claims(
            json!({ "roles": ["admin"], "exp": in_one_hour() }),
            Some(TEST_KID),
        );
        assert!(authz.verify(&token).expect("verifies").subject.is_none());

        // Empty is not an identity either.
        let token = sign_claims(
            json!({ "roles": ["admin"], "sub": "", "exp": in_one_hour() }),
            Some(TEST_KID),
        );
        assert!(authz.verify(&token).expect("verifies").subject.is_none());

        // A non-string claim must never become an identity. Which layer refuses
        // it varies — `jsonwebtoken` types the registered claims itself and
        // rejects a composite value outright, while a scalar reaches our own
        // guard — so assert the property that matters rather than the mechanism.
        for shape in [json!(42), json!(["a"]), json!(null), json!({ "a": 1 })] {
            let token = sign_claims(
                json!({ "roles": ["admin"], "sub": shape.clone(), "exp": in_one_hour() }),
                Some(TEST_KID),
            );
            let subject = authz.verify(&token).ok().and_then(|claims| claims.subject);
            assert!(
                subject.is_none(),
                "a `sub` of {shape} must not yield an identity, got {subject:?}"
            );
        }

        // A *custom* claim is not one `jsonwebtoken` knows about, so this is
        // where our own guard earns its keep: no identity, rather than a
        // stringified number.
        let mut authz = test_authorizer();
        authz.subject_claim = "tenant_id".into();
        for shape in [json!(42), json!(["a"]), json!(null)] {
            let token = sign_claims(
                json!({ "roles": ["admin"], "sub": "user-123", "tenant_id": shape.clone(), "exp": in_one_hour() }),
                Some(TEST_KID),
            );
            assert!(
                authz.verify(&token).expect("verifies").subject.is_none(),
                "a claim of {shape} must not yield a subject"
            );
        }
    }

    #[test]
    fn without_an_expected_audience_any_verifiable_token_is_accepted() {
        // The pre-existing behaviour, pinned: a token addressed to another
        // service still verifies. This is why the flag exists, and why startup
        // warns when it is unset.
        let authz = test_authorizer();
        let token = sign_claims(
            json!({ "roles": ["admin"], "sub": "u", "aud": "some-other-service", "exp": in_one_hour() }),
            Some(TEST_KID),
        );
        assert!(authz.verify(&token).is_ok());
    }

    #[test]
    fn an_expected_audience_rejects_a_token_addressed_elsewhere() {
        let mut authz = test_authorizer();
        authz.expected_audiences = vec!["oas2mcp".into()];

        let ours = sign_claims(
            json!({ "roles": ["admin"], "sub": "u", "aud": "oas2mcp", "exp": in_one_hour() }),
            Some(TEST_KID),
        );
        assert!(authz.verify(&ours).is_ok(), "our own audience must pass");

        let theirs = sign_claims(
            json!({ "roles": ["admin"], "sub": "u", "aud": "some-other-service", "exp": in_one_hour() }),
            Some(TEST_KID),
        );
        assert!(
            authz.verify(&theirs).is_err(),
            "a token minted for another service must be refused"
        );
    }

    #[test]
    fn an_expected_audience_makes_the_claim_mandatory() {
        // Otherwise the check is bypassable by simply omitting `aud`.
        let mut authz = test_authorizer();
        authz.expected_audiences = vec!["oas2mcp".into()];
        let token = sign_claims(
            json!({ "roles": ["admin"], "sub": "u", "exp": in_one_hour() }),
            Some(TEST_KID),
        );
        assert!(
            authz.verify(&token).is_err(),
            "a token with no `aud` is not addressed to us either"
        );
    }

    #[test]
    fn any_one_of_several_expected_audiences_is_enough() {
        let mut authz = test_authorizer();
        authz.expected_audiences = vec!["oas2mcp".into(), "oas2mcp-staging".into()];
        for aud in ["oas2mcp", "oas2mcp-staging"] {
            let token = sign_claims(
                json!({ "roles": ["admin"], "sub": "u", "aud": aud, "exp": in_one_hour() }),
                Some(TEST_KID),
            );
            assert!(authz.verify(&token).is_ok(), "{aud} must pass");
        }
    }

    #[test]
    fn an_audience_array_matches_when_any_entry_does() {
        // `aud` is allowed to be an array (RFC 7519 §4.1.3), which is what a
        // token minted for several services looks like.
        let mut authz = test_authorizer();
        authz.expected_audiences = vec!["oas2mcp".into()];
        let token = sign_claims(
            json!({ "roles": ["admin"], "sub": "u", "aud": ["other", "oas2mcp"], "exp": in_one_hour() }),
            Some(TEST_KID),
        );
        assert!(authz.verify(&token).is_ok());
    }

    #[test]
    fn an_expected_issuer_rejects_another_issuer() {
        let mut authz = test_authorizer();
        authz.expected_issuers = vec!["https://idp.example.com/".into()];

        let ours = sign_claims(
            json!({ "roles": ["admin"], "sub": "u", "iss": "https://idp.example.com/", "exp": in_one_hour() }),
            Some(TEST_KID),
        );
        assert!(authz.verify(&ours).is_ok());

        // A key shared between a staging and a production realm is the case this
        // catches: the JWKS verifies both, only `iss` tells them apart.
        let staging = sign_claims(
            json!({ "roles": ["admin"], "sub": "u", "iss": "https://staging-idp.example.com/", "exp": in_one_hour() }),
            Some(TEST_KID),
        );
        assert!(authz.verify(&staging).is_err());

        // And a token with no `iss` cannot satisfy the constraint either.
        let anonymous = sign_claims(
            json!({ "roles": ["admin"], "sub": "u", "exp": in_one_hour() }),
            Some(TEST_KID),
        );
        assert!(authz.verify(&anonymous).is_err());
    }

    #[test]
    fn a_token_with_no_expiry_is_refused() {
        // A bearer token that never expires is not something to accept quietly.
        // `jsonwebtoken` requires `exp` by default; pinned so a future change to
        // the validation setup cannot silently drop it.
        let authz = test_authorizer();
        let token = sign_claims(json!({ "roles": ["admin"], "sub": "u" }), Some(TEST_KID));
        assert!(authz.verify(&token).is_err());
    }

    #[test]
    fn clock_skew_widens_the_expiry_window() {
        use std::time::{SystemTime, UNIX_EPOCH};
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_secs();

        // Expired 90 seconds ago: outside the default 60s tolerance...
        let token = sign_claims(
            json!({ "roles": ["admin"], "sub": "u", "exp": now - 90 }),
            Some(TEST_KID),
        );
        let authz = test_authorizer();
        assert!(authz.verify(&token).is_err());

        // ...but inside a deliberately generous one. This is the knob for a
        // provider whose clock disagrees with ours.
        let mut authz = test_authorizer();
        authz.clock_skew = Duration::from_secs(300);
        assert!(authz.verify(&token).is_ok());
    }
}
