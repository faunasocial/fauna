"""The third-party depositor's signature over a folder deposit — the one place
the e2e depositors sign (``docs/goal/behavior/folder-deposit.md`` § The
depositor signs, the owner's device verifies).

The statement is a fixed byte layout so a remote server in any language needs
only BLAKE3 and Ed25519: the domain tag ``fauna.folder-deposit.v1\\0``, the
folder's row id as 8 big-endian bytes (two's complement), the body's length as
8 big-endian bytes, the BLAKE3 of the whole body, the BLAKE3 of the name's
UTF-8 bytes and the BLAKE3 of the declared media type's UTF-8 bytes. This
module spells it independently of the Rust ``DepositStatement`` on purpose: a
drift between the two fails the e2e deposits, as it would fail a real remote
depositor.
"""

import base64
import struct

import blake3
from cryptography.hazmat.primitives.asymmetric import ed25519

TAG = b"fauna.folder-deposit.v1\x00"

#: The HTTP door's signature header (unpadded base64url of the 64 bytes).
SIGNATURE_HEADER = "Fauna-Deposit-Signature"


def statement(folder_id: int, name: str, content_type: str, length: int, body_hash: bytes) -> bytes:
    """The exact bytes signed, over a body of ``length`` bytes whose BLAKE3 is
    ``body_hash``."""
    assert len(body_hash) == 32, "the body hash is a 32-byte BLAKE3"
    return (
        TAG
        + struct.pack(">q", folder_id)
        + struct.pack(">Q", length)
        + body_hash
        + blake3.blake3(name.encode()).digest()
        + blake3.blake3(content_type.encode()).digest()
    )


def body_hash(body: bytes) -> bytes:
    return blake3.blake3(body).digest()


def sign(key: ed25519.Ed25519PrivateKey, folder_id: int, name: str, content_type: str, body: bytes) -> bytes:
    """The depositor's 64-byte signature over ``body`` deposited into
    ``folder_id`` as ``name`` with ``content_type`` (empty = unknown)."""
    return key.sign(statement(folder_id, name, content_type, len(body), body_hash(body)))


def header_value(sig: bytes) -> str:
    """``sig`` as the HTTP door's header carries it."""
    return base64.urlsafe_b64encode(sig).rstrip(b"=").decode()
