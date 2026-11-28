//! The RSA key-length floor shared by the inbound JWKS and the signing keys.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use jsonwebtoken::jwk::{AlgorithmParameters, Jwk};

/// Smallest RSA modulus accepted unless `--min-rsa-key-bits` says otherwise:
/// NIST's minimum through 2030 (SP 800-57 Part 1).
pub const DEFAULT_MIN_RSA_KEY_BITS: u32 = 2048;

/// The bit length of an RSA JWK's modulus, or `None` for any other key type or
/// a modulus that is not valid base64url.
pub fn rsa_modulus_bits(jwk: &Jwk) -> Option<u32> {
    let AlgorithmParameters::RSA(rsa) = &jwk.algorithm else {
        return None;
    };
    let modulus = URL_SAFE_NO_PAD.decode(&rsa.n).ok()?;
    let significant = modulus.iter().position(|&byte| byte != 0)?;
    let bytes = u32::try_from(modulus.len() - significant).ok()?;
    Some(bytes * 8 - modulus[significant].leading_zeros())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn rsa_jwk(n: &[u8]) -> Jwk {
        serde_json::from_value(json!({
            "kty": "RSA",
            "n": URL_SAFE_NO_PAD.encode(n),
            "e": "AQAB",
        }))
        .expect("an RSA JWK parses")
    }

    #[test]
    fn counts_significant_bits_only() {
        // A 2048-bit modulus has its top bit set in the first of 256 bytes.
        let mut n = vec![0xff; 256];
        assert_eq!(rsa_modulus_bits(&rsa_jwk(&n)), Some(2048));
        // Leading zero octets and leading zero bits do not count.
        n[0] = 0x01;
        n.insert(0, 0);
        assert_eq!(rsa_modulus_bits(&rsa_jwk(&n)), Some(2041));
    }

    #[test]
    fn has_no_answer_for_a_non_rsa_key() {
        let ec: Jwk = serde_json::from_value(json!({
            "kty": "EC",
            "crv": "P-256",
            "x": "f83OJ3D2xF1Bg8vub9tLe1gHMzV76e8Tus9uPHvRVEU",
            "y": "x_FEzRu9m36HLN_tue659LNpXW6pCyStikYjKIWI5a0",
        }))
        .expect("an EC JWK parses");
        assert_eq!(rsa_modulus_bits(&ec), None);
    }
}
