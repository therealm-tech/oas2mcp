//! Throwaway signing keys for the unit tests, generated once per test run so no
//! private key is ever committed.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use p256::pkcs8::{EncodePrivateKey as _, EncodePublicKey as _, LineEnding};
use rsa::Generate as _;
use rsa::traits::PublicKeyParts as _;

pub struct TestKeys {
    /// The 3072-bit RSA private key, as PKCS#8 PEM.
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

/// An RSA key of a size other than the default one, for the key-length tests.
pub struct RsaKey {
    pub pem: String,
    pub path: String,
    pub n: String,
}

pub fn keys() -> &'static TestKeys {
    &KEYS
}

/// A 1024-bit RSA key: below the default minimum.
pub fn rsa_1024() -> &'static RsaKey {
    &RSA_1024
}

/// A 2048-bit RSA key: exactly the default minimum.
pub fn rsa_2048() -> &'static RsaKey {
    &RSA_2048
}

static KEYS: LazyLock<TestKeys> = LazyLock::new(|| {
    let rsa = generate_rsa(3072);

    let ec = p256::SecretKey::generate();
    let ec_pem = ec.to_pkcs8_pem(LineEnding::LF).expect("encode the EC key");
    let ec_public_pem = ec
        .public_key()
        .to_public_key_pem(LineEnding::LF)
        .expect("encode the EC public key");

    TestKeys {
        rsa_pem: rsa.pem,
        rsa_path: rsa.path,
        rsa_n: rsa.n,
        ec_path: write_private(&dir().join("ec.pem"), &ec_pem),
        ec_public_pem,
    }
});

// Apart from `KEYS`, so only the tests that use them pay for their generation.
static RSA_1024: LazyLock<RsaKey> = LazyLock::new(|| generate_rsa(1024));
static RSA_2048: LazyLock<RsaKey> = LazyLock::new(|| generate_rsa(2048));

fn generate_rsa(bits: usize) -> RsaKey {
    let key = rsa::PrivateKey::new(&mut getrandom::SysRng, bits).expect("generate an RSA key");
    let pem = key
        .to_pkcs8_pem(LineEnding::LF)
        .expect("encode the RSA key")
        .to_string();
    RsaKey {
        path: write_private(&dir().join(format!("rsa-{bits}.pem")), &pem),
        pem,
        n: URL_SAFE_NO_PAD.encode(key.n().to_be_bytes_trimmed_vartime()),
    }
}

fn dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("oas2mcp-test-keys-{}", std::process::id()));
    fs::create_dir_all(&dir).expect("create the test key directory");
    dir
}

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
