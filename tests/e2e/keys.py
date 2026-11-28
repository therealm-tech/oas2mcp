#!/usr/bin/env python3
"""Generate the suite's throwaway signing key, and everything derived from it.

One RSA key plays both signing roles: the external provider's, whose assertions
the suite mints and oas2mcp verifies, and oas2mcp's own `private_key_jwt` client
key for the document fetch. Writes into <out-dir>:

- signing-key.pem: the private key;
- jwks.json: its public half, which oas2mcp reads from disk;
- realm.json: the realm with the public key and certificate filled in.
"""

import base64
import datetime
import json
import os
import sys

from cryptography import x509
from cryptography.hazmat.primitives import hashes, serialization
from cryptography.hazmat.primitives.asymmetric import rsa
from cryptography.x509.oid import NameOID

PUBLIC_KEY_PLACEHOLDER = "__ONEACCESS_PUBLIC_KEY__"
CERTIFICATE_PLACEHOLDER = "__OAS2MCP_DOC_CERTIFICATE__"


def b64url(value: int) -> str:
    """A big integer as base64url, the way a JWK carries it."""
    raw = value.to_bytes((value.bit_length() + 7) // 8, "big")
    return base64.urlsafe_b64encode(raw).decode().rstrip("=")


def certificate(key: rsa.RSAPrivateKey) -> x509.Certificate:
    """A self-signed certificate: Keycloak's `client-jwt` takes the key in one."""
    name = x509.Name([x509.NameAttribute(NameOID.COMMON_NAME, "oas2mcp-e2e")])
    now = datetime.datetime.now(datetime.timezone.utc)
    return (
        x509.CertificateBuilder()
        .subject_name(name)
        .issuer_name(name)
        .public_key(key.public_key())
        .serial_number(x509.random_serial_number())
        .not_valid_before(now - datetime.timedelta(minutes=5))
        .not_valid_after(now + datetime.timedelta(days=1))
        .sign(key, hashes.SHA256())
    )


def write(path: str, data: str, mode: int = 0o644) -> None:
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, mode)
    with os.fdopen(fd, "w") as handle:
        handle.write(data)


def main() -> int:
    if len(sys.argv) != 4:
        print(f"usage: {sys.argv[0]} <realm-template.json> <out-dir> <kid>", file=sys.stderr)
        return 2
    template, out, kid = sys.argv[1:]

    key = rsa.generate_private_key(public_exponent=65537, key_size=2048)
    pem = key.private_bytes(
        serialization.Encoding.PEM,
        serialization.PrivateFormat.PKCS8,
        serialization.NoEncryption(),
    ).decode()
    write(os.path.join(out, "signing-key.pem"), pem, 0o600)

    numbers = key.public_key().public_numbers()
    jwk = {"kty": "RSA", "use": "sig", "alg": "RS256", "kid": kid,
           "n": b64url(numbers.n), "e": b64url(numbers.e)}
    write(os.path.join(out, "jwks.json"), json.dumps({"keys": [jwk]}, indent=2))

    spki = key.public_key().public_bytes(
        serialization.Encoding.DER, serialization.PublicFormat.SubjectPublicKeyInfo
    )
    der = certificate(key).public_bytes(serialization.Encoding.DER)
    with open(template) as handle:
        realm = handle.read()
    for placeholder in (PUBLIC_KEY_PLACEHOLDER, CERTIFICATE_PLACEHOLDER):
        if placeholder not in realm:
            print(f"{template} has no {placeholder} to fill", file=sys.stderr)
            return 1
    realm = realm.replace(PUBLIC_KEY_PLACEHOLDER, base64.b64encode(spki).decode())
    realm = realm.replace(CERTIFICATE_PLACEHOLDER, base64.b64encode(der).decode())
    write(os.path.join(out, "realm.json"), realm)
    return 0


if __name__ == "__main__":
    sys.exit(main())
