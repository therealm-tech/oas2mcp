#!/usr/bin/env python3
"""Generate the suite's throwaway keys, and everything derived from them.

One RSA key plays both signing roles: the external provider's, whose assertions
the suite mints and oas2mcp verifies, and oas2mcp's own `private_key_jwt` client
key for the document fetch. Writes into <out-dir>:

- signing-key.pem: the private key;
- jwks.json: its public half, which oas2mcp reads from disk;
- realm.json: the realm with the public key and certificate filled in.

Apart from that, the TLS material of the sandbox API, under a CA of its own:
ca.pem, api-cert.pem and api-key.pem, the certificate naming each <api-host>.
"""

import base64
import datetime
import json
import os
import sys

from cryptography import x509
from cryptography.hazmat.primitives import hashes, serialization
from cryptography.hazmat.primitives.asymmetric import rsa
from cryptography.x509.oid import ExtendedKeyUsageOID, NameOID

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


def tls(out: str, hosts: list[str]) -> None:
    """A CA, and a server certificate it signs for `hosts`."""
    now = datetime.datetime.now(datetime.timezone.utc)
    ca_key = rsa.generate_private_key(public_exponent=65537, key_size=2048)
    leaf_key = rsa.generate_private_key(public_exponent=65537, key_size=2048)
    ca_name = x509.Name([x509.NameAttribute(NameOID.COMMON_NAME, "oas2mcp-e2e CA")])

    def builder(subject: x509.Name, key: rsa.RSAPrivateKey) -> x509.CertificateBuilder:
        return (
            x509.CertificateBuilder()
            .subject_name(subject)
            .issuer_name(ca_name)
            .public_key(key.public_key())
            .serial_number(x509.random_serial_number())
            .not_valid_before(now - datetime.timedelta(minutes=5))
            .not_valid_after(now + datetime.timedelta(days=1))
            .add_extension(x509.SubjectKeyIdentifier.from_public_key(key.public_key()), False)
            .add_extension(
                x509.AuthorityKeyIdentifier.from_issuer_public_key(ca_key.public_key()), False
            )
        )

    ca = (
        builder(ca_name, ca_key)
        .add_extension(x509.BasicConstraints(ca=True, path_length=0), True)
        .add_extension(
            x509.KeyUsage(
                digital_signature=False, content_commitment=False, key_encipherment=False,
                data_encipherment=False, key_agreement=False, key_cert_sign=True,
                crl_sign=True, encipher_only=False, decipher_only=False,
            ),
            True,
        )
        .sign(ca_key, hashes.SHA256())
    )
    leaf = (
        builder(x509.Name([x509.NameAttribute(NameOID.COMMON_NAME, hosts[0])]), leaf_key)
        .add_extension(x509.BasicConstraints(ca=False, path_length=None), True)
        .add_extension(x509.SubjectAlternativeName([x509.DNSName(h) for h in hosts]), False)
        .add_extension(x509.ExtendedKeyUsage([ExtendedKeyUsageOID.SERVER_AUTH]), False)
        .sign(ca_key, hashes.SHA256())
    )
    pem = serialization.Encoding.PEM
    write(os.path.join(out, "ca.pem"), ca.public_bytes(pem).decode())
    write(os.path.join(out, "api-cert.pem"), leaf.public_bytes(pem).decode())
    write(os.path.join(out, "api-key.pem"), private_pem(leaf_key), 0o600)


def private_pem(key: rsa.RSAPrivateKey) -> str:
    return key.private_bytes(
        serialization.Encoding.PEM,
        serialization.PrivateFormat.PKCS8,
        serialization.NoEncryption(),
    ).decode()


def write(path: str, data: str, mode: int = 0o644) -> None:
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, mode)
    with os.fdopen(fd, "w") as handle:
        handle.write(data)


def main() -> int:
    if len(sys.argv) < 5:
        print(
            f"usage: {sys.argv[0]} <realm-template.json> <out-dir> <kid> <api-host>...",
            file=sys.stderr,
        )
        return 2
    template, out, kid, *api_hosts = sys.argv[1:]
    tls(out, api_hosts)

    key = rsa.generate_private_key(public_exponent=65537, key_size=2048)
    write(os.path.join(out, "signing-key.pem"), private_pem(key), 0o600)

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
