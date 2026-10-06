"""The harness's one writer of a recipient seal key.

A recipient's standing seal key has two public halves, both derived from the
account's MSEK: the X25519 key and the ML-KEM-768 encapsulation key. The wire
call that publishes them (``fauna.bridges.provision_recipient_mls_pubkey``)
requires both, as every app sends them, so mail to the recipient is sealed
under the hybrid suite. Every harness caller provisions through this module, so
no test can publish a shape the apps never do.

The halves come from the test-only seal helper, which wraps the same shared-Rust
derivation the apps run. A caller that later opens what was sealed keeps the
returned MSEK (the MLS snapshot the MDA opens bodies with is sealed from it, and
carries the decapsulation half). A caller that only needs *a* key on file
passes no MSEK and drops the result.
"""

from __future__ import annotations

import base64
import secrets
from dataclasses import dataclass
from typing import Callable

from helpers.shared_identity import SEAL_KEY_WRITE_KIND

#: ML-KEM-768 encapsulation-key length (`fauna_pq_kem::MLKEM768_ENCAPS_KEY_LEN`).
MLKEM_EK_LEN = 1184

SealHelper = Callable[[str, dict], bytes]


@dataclass(frozen=True)
class RecipientSealKey:
    """An MSEK and the two public halves of the recipient seal key it derives."""

    msek: bytes
    mls_pubkey: bytes
    mlkem_ek: bytes

    def request(self, actor_id: bytes) -> dict:
        """The provision request publishing this key for ``actor_id``."""
        return {
            "actor_id": actor_id,
            "mls_pubkey": self.mls_pubkey,
            "mlkem_ek": self.mlkem_ek,
        }


def _session_seal_helper() -> SealHelper:
    """The session's seal helper, for a caller that holds no fixture for it."""
    import conftest

    binary = conftest._ensure_seal_helper_built()
    return lambda subcommand, params: conftest._run_seal_helper(binary, subcommand, params)


def derive_recipient_seal_key(
    msek: bytes | None = None, *, run_seal_helper: SealHelper | None = None
) -> RecipientSealKey:
    """Derive both public halves from ``msek`` (a fresh random one when absent)."""
    run = run_seal_helper or _session_seal_helper()
    msek = msek if msek is not None else secrets.token_bytes(32)
    params = {"msek_b64": base64.b64encode(msek).decode()}
    mls_pubkey = run("derive-recipient-pubkey", params)
    mlkem_ek = run("derive-recipient-mlkem-ek", params)
    assert len(mls_pubkey) == 32, f"recipient pubkey must be 32 bytes, got {len(mls_pubkey)}"
    assert len(mlkem_ek) == MLKEM_EK_LEN, (
        f"recipient ML-KEM ek must be {MLKEM_EK_LEN} bytes, got {len(mlkem_ek)}"
    )
    return RecipientSealKey(msek=msek, mls_pubkey=mls_pubkey, mlkem_ek=mlkem_ek)


def provision_recipient_seal_key(
    ws,
    actor_id: bytes,
    *,
    msek: bytes | None = None,
    run_seal_helper: SealHelper | None = None,
) -> RecipientSealKey:
    """Publish a hybrid recipient seal key for ``actor_id`` over ``ws``.

    ``ws`` is an open WS-RPC client authenticated as the actor itself or as an
    admin (the two callers the nest admits). Returns the key, MSEK included.
    """
    key = derive_recipient_seal_key(msek, run_seal_helper=run_seal_helper)
    ws.call(SEAL_KEY_WRITE_KIND, key.request(actor_id))
    return key
