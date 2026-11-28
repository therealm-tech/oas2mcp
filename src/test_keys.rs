//! Throwaway signing keys for the unit tests, generated once per test run so no
//! private key is ever committed.

use std::fs;
use std::path::Path;
use std::sync::LazyLock;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use p256::pkcs8::{EncodePrivateKey as _, EncodePublicKey as _, LineEnding};
use rsa::Generate as _;
use rsa::traits::PublicKeyParts as _;

pub struct TestKeys {
    /// The RSA private key, as PKCS#8 PEM.
    pub rsa_pem: String,
    /// Where the RSA private key is written, for code that loads it from disk.
    pub rsa_path: String,
    /// The RSA modulus, base64url-encoded as a JWK carries it.
    pub rsa_n: String,
    /// Where the P-256 private key is written.
    pub ec_path: String,
    /// The P-256 public key, as SPKI PEM.
    pub ec_public_pem: String,
}

pub fn keys() -> &'static TestKeys {
    &KEYS
}

static KEYS: LazyLock<TestKeys> = LazyLock::new(|| {
    let dir = std::env::temp_dir().join(format!("oas2mcp-test-keys-{}", std::process::id()));
    fs::create_dir_all(&dir).expect("create the test key directory");

    let rsa = rsa::PrivateKey::generate();
    let rsa_pem = rsa
        .to_pkcs8_pem(LineEnding::LF)
        .expect("encode the RSA key")
        .to_string();
    let rsa_n = URL_SAFE_NO_PAD.encode(rsa.n().to_be_bytes_trimmed_vartime());

    let ec = p256::SecretKey::generate();
    let ec_pem = ec.to_pkcs8_pem(LineEnding::LF).expect("encode the EC key");
    let ec_public_pem = ec
        .public_key()
        .to_public_key_pem(LineEnding::LF)
        .expect("encode the EC public key");

    TestKeys {
        rsa_path: write_private(&dir.join("rsa.pem"), &rsa_pem),
        rsa_pem,
        rsa_n,
        ec_path: write_private(&dir.join("ec.pem"), &ec_pem),
        ec_public_pem,
    }
});

/// Write a key readable by its owner only, as the key loader expects.
fn write_private(path: &Path, pem: &str) -> String {
    fs::write(path, pem).expect("write the test key");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))
            .expect("restrict the test key");
    }
    path.display().to_string()
}
