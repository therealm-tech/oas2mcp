#!/usr/bin/env python3
"""Print the JWKS for a PEM private key, so oas2mcp can verify what it signed.

Used for the external provider's key: oas2mcp reads the key set from disk, which
saves the suite from running an HTTP server whose only job would be to serve two
public numbers.
"""

import base64
import json
import sys

from cryptography.hazmat.primitives import serialization
from cryptography.hazmat.primitives.asymmetric import rsa


def b64(value: int) -> str:
    """A big integer as base64url, the way a JWK carries it."""
    raw = value.to_bytes((value.bit_length() + 7) // 8, "big")
    return base64.urlsafe_b64encode(raw).decode().rstrip("=")


def main() -> int:
    if len(sys.argv) != 3:
        print(f"usage: {sys.argv[0]} <private-key.pem> <kid>", file=sys.stderr)
        return 2

    with open(sys.argv[1], "rb") as handle:
        key = serialization.load_pem_private_key(handle.read(), password=None)
    if not isinstance(key, rsa.RSAPrivateKey):
        print("only RSA keys are supported here", file=sys.stderr)
        return 2

    numbers = key.public_key().public_numbers()
    print(
        json.dumps(
            {
                "keys": [
                    {
                        "kty": "RSA",
                        "use": "sig",
                        "alg": "RS256",
                        "kid": sys.argv[2],
                        "n": b64(numbers.n),
                        "e": b64(numbers.e),
                    }
                ]
            },
            indent=2,
        )
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
