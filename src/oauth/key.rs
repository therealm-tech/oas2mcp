//! Loading of the private key that signs RFC 7523 client assertions.
//!
//! The key is read once at startup from a PEM file. It is never logged, never
//! echoed into an error message, and never accepted from an environment
//! variable — only a path to a file is configurable, so the key material stays
//! wherever the operator put it.

use std::fs;
use std::path::Path;

use anyhow::{Context as _, bail};
use jsonwebtoken::jwk::Jwk;
use jsonwebtoken::{Algorithm, EncodingKey};

use crate::cli::SigningAlg;
use crate::key_length::rsa_modulus_bits;

/// A private key ready to sign client assertions, with the algorithm and
/// optional `kid` that go on every header signed with it.
pub struct SigningKey {
    pub(super) algorithm: Algorithm,
    pub(super) key_id: Option<String>,
    pub(super) key: EncodingKey,
}

/// Which PEM parser reads a given algorithm's key.
enum Family {
    Rsa,
    Ec,
    Ed,
}

/// Read and parse the signing key at `path`, refusing an RSA key whose modulus
/// is shorter than `min_rsa_bits`.
pub fn load(
    path: &Path,
    alg: SigningAlg,
    key_id: Option<String>,
    min_rsa_bits: u32,
) -> anyhow::Result<SigningKey> {
    let pem = fs::read(path).with_context(|| {
        format!(
            "reading the OAuth client private key from {}",
            path.display()
        )
    })?;
    warn_on_loose_permissions(path);

    let algorithm = algorithm_for(alg);
    // The parser is chosen by key family: handing an EC key to the RSA parser
    // fails with an opaque error, so the algorithm decides up front and the
    // error message can name what was expected.
    let family = family(alg);
    let key = match family {
        Family::Rsa => EncodingKey::from_rsa_pem(&pem),
        Family::Ec => EncodingKey::from_ec_pem(&pem),
        Family::Ed => EncodingKey::from_ed_pem(&pem),
    }
    .with_context(|| {
        format!(
            "parsing {} as a {alg} signing key (PKCS#8 PEM expected)",
            path.display()
        )
    })?;
    if matches!(family, Family::Rsa) {
        check_rsa_length(path, &key, algorithm, min_rsa_bits)?;
    }

    // Path, algorithm and `kid` only — never anything derived from the key.
    tracing::debug!(
        path = %path.display(),
        alg = %alg,
        kid = key_id.as_deref().unwrap_or("-"),
        "loaded the OAuth client signing key"
    );
    Ok(SigningKey {
        algorithm,
        key_id,
        key,
    })
}

/// Refuse an RSA signing key shorter than `min_bits`.
fn check_rsa_length(
    path: &Path,
    key: &EncodingKey,
    algorithm: Algorithm,
    min_bits: u32,
) -> anyhow::Result<()> {
    let bits = Jwk::from_encoding_key(key, algorithm)
        .ok()
        .as_ref()
        .and_then(rsa_modulus_bits)
        .with_context(|| format!("reading the RSA modulus of {}", path.display()))?;
    if bits < min_bits {
        bail!(
            "{} holds a {bits}-bit RSA key, below the {min_bits}-bit minimum; rotate it to a \
             longer key, or lower --min-rsa-key-bits (MIN_RSA_KEY_BITS) to accept it",
            path.display()
        );
    }
    Ok(())
}

/// The `jsonwebtoken` algorithm behind a CLI choice.
fn algorithm_for(alg: SigningAlg) -> Algorithm {
    match alg {
        SigningAlg::Rs256 => Algorithm::RS256,
        SigningAlg::Rs384 => Algorithm::RS384,
        SigningAlg::Rs512 => Algorithm::RS512,
        SigningAlg::Ps256 => Algorithm::PS256,
        SigningAlg::Ps384 => Algorithm::PS384,
        SigningAlg::Ps512 => Algorithm::PS512,
        SigningAlg::Es256 => Algorithm::ES256,
        SigningAlg::Es384 => Algorithm::ES384,
        SigningAlg::EdDsa => Algorithm::EdDSA,
    }
}

/// The key family an algorithm signs with.
fn family(alg: SigningAlg) -> Family {
    match alg {
        SigningAlg::Rs256
        | SigningAlg::Rs384
        | SigningAlg::Rs512
        | SigningAlg::Ps256
        | SigningAlg::Ps384
        | SigningAlg::Ps512 => Family::Rsa,
        SigningAlg::Es256 | SigningAlg::Es384 => Family::Ec,
        SigningAlg::EdDsa => Family::Ed,
    }
}

/// Whether a Unix mode grants any access to the world.
///
/// Group access is deliberately tolerated: a Kubernetes `Secret` volume is
/// owned by root and readable by `fsGroup`, so `0440` is how a non-root
/// container legitimately reads its own key. World access has no such excuse —
/// and `0644` is what you get by forgetting `defaultMode` entirely.
fn world_readable(mode: u32) -> bool {
    mode & 0o007 != 0
}

/// Warn when the key file is readable by the whole world. Only a warning: it
/// still works, but this key is a credential and the operator should know it is
/// lying around in the open.
#[cfg(unix)]
fn warn_on_loose_permissions(path: &Path) {
    use std::os::unix::fs::PermissionsExt as _;

    let Ok(metadata) = fs::metadata(path) else {
        return;
    };
    let mode = metadata.permissions().mode() & 0o777;
    if world_readable(mode) {
        tracing::warn!(
            path = %path.display(),
            mode = format!("{mode:04o}"),
            "the OAuth client private key is world-readable; tighten it to 0400 (or 0440 for a \
             Kubernetes Secret volume read via fsGroup)"
        );
    }
}

#[cfg(not(unix))]
fn warn_on_loose_permissions(_path: &Path) {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::key_length::DEFAULT_MIN_RSA_KEY_BITS;
    use crate::test_keys::{keys, rsa_1024, rsa_2048};

    const MIN: u32 = DEFAULT_MIN_RSA_KEY_BITS;

    /// Assert the load failed and hand back the formatted error.
    ///
    /// `expect_err` would need `SigningKey: Debug`, and it deliberately has no
    /// `Debug` impl: a derived one would put key material one `{:?}` away from
    /// a log line.
    fn rejection(result: anyhow::Result<SigningKey>) -> String {
        match result {
            Ok(_) => panic!("expected the key to be rejected"),
            Err(err) => format!("{err:#}"),
        }
    }

    #[test]
    fn loads_an_rsa_key_and_keeps_the_key_id() {
        let key = load(
            Path::new(&keys().rsa_path),
            SigningAlg::Rs256,
            Some("kid-1".to_string()),
            MIN,
        )
        .expect("the RSA test key loads");
        assert_eq!(key.algorithm, Algorithm::RS256);
        assert_eq!(key.key_id.as_deref(), Some("kid-1"));
    }

    #[test]
    fn loads_an_ec_key_through_the_ec_parser() {
        // The whole point of picking a parser per family: an EC key must load
        // for `es256` and fail for `rs256`, not silently misbehave.
        let key = load(Path::new(&keys().ec_path), SigningAlg::Es256, None, MIN)
            .expect("the EC test key loads as es256");
        assert_eq!(key.algorithm, Algorithm::ES256);
        assert!(key.key_id.is_none());

        let message = rejection(load(
            Path::new(&keys().ec_path),
            SigningAlg::Rs256,
            None,
            MIN,
        ));
        assert!(message.contains("as a rs256 signing key"), "{message}");
    }

    #[test]
    fn a_1024_bit_rsa_key_stops_startup_by_default() {
        let message = rejection(load(
            Path::new(&rsa_1024().path),
            SigningAlg::Rs256,
            None,
            MIN,
        ));
        assert!(
            message.contains("1024-bit RSA key, below the 2048-bit minimum"),
            "{message}"
        );
        // The way out is named, so the operator need not dig through the docs.
        assert!(message.contains("--min-rsa-key-bits"), "{message}");
        assert!(message.contains("MIN_RSA_KEY_BITS"), "{message}");
    }

    #[test]
    fn a_1024_bit_rsa_key_loads_once_the_minimum_is_lowered() {
        load(Path::new(&rsa_1024().path), SigningAlg::Ps256, None, 1024)
            .expect("a lowered minimum accepts the 1024-bit key");
    }

    #[test]
    fn rsa_keys_of_2048_and_3072_bits_meet_the_default_minimum() {
        for path in [&rsa_2048().path, &keys().rsa_path] {
            load(Path::new(path), SigningAlg::Rs256, None, MIN).expect("the key loads");
        }
    }

    #[test]
    fn the_minimum_leaves_ec_keys_alone() {
        load(Path::new(&keys().ec_path), SigningAlg::Es256, None, 4096)
            .expect("an EC key is not measured against the RSA minimum");
    }

    #[test]
    fn a_missing_key_file_names_the_path() {
        let message = rejection(load(
            Path::new("/definitely/not/a/key.pem"),
            SigningAlg::Rs256,
            None,
            MIN,
        ));
        assert!(
            message.contains("reading the OAuth client private key"),
            "{message}"
        );
        assert!(message.contains("/definitely/not/a/key.pem"), "{message}");
    }

    #[test]
    fn garbage_is_rejected_without_echoing_the_file() {
        // The PEM banner is assembled rather than written out: spelled in full,
        // even in a comment, it trips the `detect-private-key` pre-commit hook —
        // which stays armed on this file precisely because it is the one that
        // handles keys.
        let banner = ["-----BEGIN", "PRIVATE", "KEY-----"].join(" ");
        let path = std::env::temp_dir().join("oas2mcp-not-a-key.pem");
        fs::write(&path, format!("{banner}\nZ2FyYmFnZQ==\n")).expect("write temp file");
        let message = rejection(load(&path, SigningAlg::Rs256, None, MIN));
        let _ = fs::remove_file(&path);

        assert!(message.contains("PKCS#8 PEM expected"), "{message}");
        // Whatever went wrong, the file's contents must not travel in the error:
        // for a real key that would be the credential itself.
        assert!(!message.contains("Z2FyYmFnZQ"), "{message}");
    }

    #[test]
    fn only_world_access_is_flagged() {
        assert!(!world_readable(0o400));
        assert!(!world_readable(0o600));
        // The mode a Kubernetes Secret volume needs for a non-root container to
        // read it via fsGroup — warning here would cry wolf on every pod.
        assert!(!world_readable(0o440));
        // The mode you get by forgetting `defaultMode`.
        assert!(world_readable(0o644));
        assert!(world_readable(0o004));
    }
}
