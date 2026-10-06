"""Fixture setup for journeys that mint content trust on a **dedicated** nest
(`docs/goal/ui/nests.md` § Trust facet — grants): sign the app in as that
nest's admin, and enroll the `mda` holder a mail grant is sealed to.

Convention 8 carve-out (b): both are arranging preconditions — a signed-in
owner and a nest that has a mail service to trust — not the mutation under
test. Shared by the Nests-page grant journeys (`test_nest_trust_grants.py`)
and the labeler-catalog subscription journey (`test_labeler_catalog.py`),
whose subscribe mints the same kind of grant to the same holder.
"""

from __future__ import annotations

import sqlite3


def login_as_admin(app, nest, *, device_id: str) -> None:
    """Point the app at `nest` and sign in as its admin (Admin ⊇ User covers
    the tier-create, the mail enable and the mint). `device_id` names this
    journey's device so two journeys never share one device row."""
    from conftest import _relaunch_trusting_nest

    _relaunch_trusting_nest(app.driver, nest)
    admin = nest["admin"]
    app.driver.set_state({
        "session": {
            "authenticated": True,
            "node_url": nest["url"],
            "secret_hex": bytes(admin["signing_key"]).hex(),
            "handle": "admin",
            "actor_id": admin["actor_id_hex"],
            "device_id": device_id,
        },
        "nav": {"stack": [{"view": "feed"}]},
    })
    # No settle-sleep: every next step is a page navigation whose own
    # element wait is the barrier (convention 14).


def seed_mda_holder(nest, *, bridge_id: str = "trust-mda") -> bytes:
    """Enroll an approved `mda`-role content-processor holder with an X25519
    seal target on `nest`, without running a bridge — the same admin
    pre-approve + x25519 attest `_spawn_mda_bridge` performs before its
    process starts. Returns the holder's X25519 public key.

    The mint only needs a holder to SEAL to; the nest stores the deposit and
    the test reads it back, so no holder process has to fetch anything."""
    from cryptography.hazmat.primitives.asymmetric.x25519 import X25519PrivateKey
    from cryptography.hazmat.primitives.serialization import Encoding, PublicFormat
    from nacl.signing import SigningKey

    from helpers.bridge_enrollment import enroll_and_approve_bridge

    ed_pub = bytes(SigningKey.generate().verify_key)
    x_pub = X25519PrivateKey.generate().public_key().public_bytes(
        Encoding.Raw, PublicFormat.Raw
    )
    enroll_and_approve_bridge(nest["url"], nest["admin"]["signing_key"], ed_pub, "mda", bridge_id)
    conn = sqlite3.connect(nest["db_path"], timeout=10.0)
    try:
        conn.execute(
            "UPDATE bridge_service_users SET x25519_pubkey = ? WHERE ed25519_pubkey = ?",
            (x_pub, ed_pub),
        )
        conn.commit()
    finally:
        conn.close()
    return x_pub
