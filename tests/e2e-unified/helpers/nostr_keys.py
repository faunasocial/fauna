"""A Nostr keypair minted in the harness, independent of the code under test.

A test that links a key the user *brought* (`docs/goal/ui/nostr.md` § Layout &
flow — the import mode) must know, before the app ever sees it, which public
key that secret belongs to: asserting only "the page says linked" would pass
just as well for an app that ignored the pasted secret and generated a fresh
one. So the expected npub is derived here, from first principles — the
secp256k1 x-only public key (BIP-340) of the secret, bech32-encoded per NIP-19
— with nothing but the standard library, never through `fauna_ffi` or any
other build of the crate whose behavior is under test.
"""
from __future__ import annotations

import secrets

# secp256k1 domain parameters (SEC 2 § 2.4.1).
_P = 0xFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFEFFFFFC2F
_N = 0xFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFEBAAEDCE6AF48A03BBFD25E8CD0364141
_G = (
    0x79BE667EF9DCBBAC55A06295CE870B07029BFCDB2DCE28D959F2815B16F81798,
    0x483ADA7726A3C4655DA4FBFC0E1108A8FD17B448A68554199C47D08FFB10D4B8,
)


def _add(a, b):
    if a is None:
        return b
    if b is None:
        return a
    if a[0] == b[0] and (a[1] + b[1]) % _P == 0:
        return None
    if a == b:
        lam = 3 * a[0] * a[0] * pow(2 * a[1], -1, _P) % _P
    else:
        lam = (b[1] - a[1]) * pow(b[0] - a[0], -1, _P) % _P
    x = (lam * lam - a[0] - b[0]) % _P
    return x, (lam * (a[0] - x) - a[1]) % _P


def _mul(k: int):
    point, addend = None, _G
    while k:
        if k & 1:
            point = _add(point, addend)
        addend = _add(addend, addend)
        k >>= 1
    return point


_CHARSET = "qpzry9x8gf2tvdw0s3jn54khce6mua7l"


def _polymod(values) -> int:
    gen = (0x3B6A57B2, 0x26508E6D, 0x1EA119FA, 0x3D4233DD, 0x2A1462B3)
    chk = 1
    for v in values:
        top = chk >> 25
        chk = (chk & 0x1FFFFFF) << 5 ^ v
        for i in range(5):
            chk ^= gen[i] if (top >> i) & 1 else 0
    return chk


def _bech32(hrp: str, data: bytes) -> str:
    acc, bits, five = 0, 0, []
    for byte in data:
        acc = (acc << 8) | byte
        bits += 8
        while bits >= 5:
            bits -= 5
            five.append((acc >> bits) & 31)
    if bits:
        five.append((acc << (5 - bits)) & 31)
    expanded = [ord(c) >> 5 for c in hrp] + [0] + [ord(c) & 31 for c in hrp]
    pm = _polymod(expanded + five + [0] * 6) ^ 1
    checksum = [(pm >> 5 * (5 - i)) & 31 for i in range(6)]
    return hrp + "1" + "".join(_CHARSET[d] for d in five + checksum)


def new_keypair() -> tuple[str, str, str]:
    """A fresh random keypair as ``(nsec, npub, pubkey_hex)``."""
    while True:
        secret = int.from_bytes(secrets.token_bytes(32), "big")
        if 0 < secret < _N:
            break
    x_only = _mul(secret)[0].to_bytes(32, "big")
    return (
        _bech32("nsec", secret.to_bytes(32, "big")),
        _bech32("npub", x_only),
        x_only.hex(),
    )
