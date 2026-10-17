"""NIP-57 zap receipts minted outside any nest — a tipper's wallet and the
Lightning provider that reports the payment.

A tip reaches a nest as a kind-9735 *receipt* signed by the payee's Lightning
provider, embedding the tipper's kind-9734 *request* in its ``description``
tag. The nest believes it only when the provider's key is one the payee
designated (`docs/goal/behavior/monetization.md` § Zap receipts — the trust
model), so a test that wants a credited tip designates a signer it holds and
signs the receipt with it here. Signing goes through ``fauna_ffi``'s harness
Nostr seam, the same one ``tests/api/test_nostr_nest.py`` signs far authors'
notes with.
"""

from __future__ import annotations

import json
import os
import time

import fauna_ffi

#: A 21-sat BOLT-11 amount: ``n`` multiplies by 100 msats, so ``210n`` is
#: 21_000 msats. Nothing in the system settles it against a real payment —
#: the designated signer's word is the whole trust model.
LNBC_21_SATS = "lnbc210n1pjfake"


class ZapSigner:
    """A Lightning provider's receipt-signing key."""

    def __init__(self) -> None:
        self.secret = os.urandom(32)
        self.pubkey: str = fauna_ffi.nostr_keys(self.secret)["pubkey"]

    def receipt(
        self,
        *,
        payee_pubkey: str,
        target_event_id: str,
        tipper_secret: bytes,
        bolt11: str = LNBC_21_SATS,
    ) -> dict:
        """A signed kind-9735 receipt for a tip from ``tipper_secret``'s key to
        ``payee_pubkey`` on the event ``target_event_id``."""
        now = int(time.time())
        request = fauna_ffi.nostr_sign_event(tipper_secret, {
            "kind": 9734, "created_at": now,
            "tags": [["p", payee_pubkey]], "content": "",
        })
        return fauna_ffi.nostr_sign_event(self.secret, {
            "kind": 9735, "created_at": now,
            "tags": [
                ["p", payee_pubkey],
                ["e", target_event_id],
                ["description", json.dumps(request)],
                ["bolt11", bolt11],
            ],
            "content": "",
        })
